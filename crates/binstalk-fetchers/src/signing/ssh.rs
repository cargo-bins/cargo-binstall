//! [sshsig] signatures, as made by `ssh-keygen -Y sign -n file`.
//!
//! [sshsig]: https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig

use aws_lc_rs::{
    digest,
    signature::{self, EcdsaVerificationAlgorithm, RsaPublicKeyComponents, UnparsedPublicKey},
};
use base64::prelude::{BASE64_STANDARD, BASE64_STANDARD_NO_PAD, Engine};
use binstalk_downloader::download::DataVerifier;
use bytes::Bytes;
use tracing::{error, trace};

use crate::FetchError;

const MAGIC: &[u8] = b"SSHSIG";

/// The namespace binstall accepts. `ssh-keygen` documents `file` as the
/// namespace for signing files, and checking it stops a signature made for
/// another purpose (a git commit, say) from being reused for a package.
const NAMESPACE: &[u8] = b"file";

pub struct SshVerifier {
    key: PublicKey,
    /// SHA256 fingerprint of the key, as `ssh-keygen -l` prints it.
    fingerprint: String,
    reserved: Vec<u8>,
    hash: Hash,
    signature: Vec<u8>,
}

impl SshVerifier {
    pub fn new(pubkey: &str, signature: &[u8]) -> Result<Self, FetchError> {
        trace!(key=?pubkey, "parsing ssh public key");
        let key_blob = parse_public_key_line(pubkey).ok_or_else(|| {
            error!("Package public key is not a valid OpenSSH public key");
            FetchError::InvalidSignature
        })?;
        let key = PublicKey::parse(&key_blob).ok_or_else(|| {
            error!(
                "Package public key is not a supported ssh-ed25519, ecdsa-sha2-* or ssh-rsa key"
            );
            FetchError::InvalidSignature
        })?;

        trace!(?signature, "parsing ssh signature");
        let sig = parse_armored_signature(signature).ok_or_else(|| {
            error!("Signature file is not a valid SSH signature");
            FetchError::InvalidSignature
        })?;

        if sig.public_key != key_blob {
            error!("Signature was made by a different SSH key than the package's public key");
            return Err(FetchError::InvalidSignature);
        }
        if sig.namespace != NAMESPACE {
            error!(
                "Signature has namespace {:?}, expected \"file\" (`ssh-keygen -Y sign -n file`)",
                String::from_utf8_lossy(&sig.namespace)
            );
            return Err(FetchError::InvalidSignature);
        }
        let hash = Hash::from_name(&sig.hash_algorithm).ok_or_else(|| {
            error!(
                "Signature uses unsupported hash algorithm {:?}",
                String::from_utf8_lossy(&sig.hash_algorithm)
            );
            FetchError::InvalidSignature
        })?;

        let fingerprint = BASE64_STANDARD_NO_PAD.encode(digest::digest(&digest::SHA256, &key_blob));

        Ok(Self {
            key,
            fingerprint,
            reserved: sig.reserved,
            hash,
            signature: sig.signature,
        })
    }

    pub fn data_verifier(&self) -> Box<dyn DataVerifier + '_> {
        Box::new(SshDataVerifier {
            verifier: self,
            context: Some(digest::Context::new(self.hash.algorithm())),
        })
    }

    pub fn info(&self) -> String {
        format!("{} key SHA256:{}", self.key.name(), self.fingerprint)
    }

    /// Verify the signature over the message whose hash is `message_hash`.
    fn verify(&self, message_hash: &[u8]) -> bool {
        let mut signed = Vec::with_capacity(64 + message_hash.len());
        signed.extend_from_slice(MAGIC);
        put_string(&mut signed, NAMESPACE);
        put_string(&mut signed, &self.reserved);
        put_string(&mut signed, self.hash.name());
        put_string(&mut signed, message_hash);

        let verified = (|| {
            let mut reader = Reader(&self.signature);
            let format = reader.string()?;
            let blob = reader.string()?;
            reader.finish()?;
            self.key.verify(format, blob, &signed)
        })();

        match verified {
            Some(Ok(())) => true,
            Some(Err(_)) => {
                error!("SSH signature does not match the package");
                false
            }
            None => {
                error!("SSH signature is malformed or uses an unexpected format");
                false
            }
        }
    }
}

struct SshDataVerifier<'a> {
    verifier: &'a SshVerifier,
    context: Option<digest::Context>,
}

impl DataVerifier for SshDataVerifier<'_> {
    fn update(&mut self, data: &Bytes) {
        if let Some(context) = &mut self.context {
            context.update(data);
        }
    }

    fn validate(&mut self) -> bool {
        self.context
            .take()
            .is_some_and(|context| self.verifier.verify(context.finish().as_ref()))
    }
}

#[derive(Clone, Copy, Debug)]
enum Hash {
    Sha256,
    Sha512,
}

impl Hash {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"sha256" => Some(Self::Sha256),
            b"sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static [u8] {
        match self {
            Self::Sha256 => b"sha256",
            Self::Sha512 => b"sha512",
        }
    }

    fn algorithm(self) -> &'static digest::Algorithm {
        match self {
            Self::Sha256 => &digest::SHA256,
            Self::Sha512 => &digest::SHA512,
        }
    }
}

#[derive(Debug)]
enum PublicKey {
    Ed25519(Vec<u8>),
    Ecdsa {
        name: &'static str,
        algorithm: &'static EcdsaVerificationAlgorithm,
        /// Size of each of the two signature scalars.
        scalar_len: usize,
        /// SEC1 uncompressed point.
        point: Vec<u8>,
    },
    Rsa {
        e: Vec<u8>,
        n: Vec<u8>,
    },
}

impl PublicKey {
    /// Parse the key from its SSH wire encoding (RFC 4253 section 6.6,
    /// RFC 5656 section 3.1).
    fn parse(blob: &[u8]) -> Option<Self> {
        let mut reader = Reader(blob);
        let key = match reader.string()? {
            b"ssh-ed25519" => {
                let key = reader.string()?;
                (key.len() == 32).then(|| Self::Ed25519(key.to_vec()))?
            }
            b"ssh-rsa" => {
                let e = strip_mpint(reader.string()?)?.to_vec();
                let n = strip_mpint(reader.string()?)?.to_vec();
                Self::Rsa { e, n }
            }
            name => {
                let (name, curve, algorithm, scalar_len): (_, &[u8], _, _) = match name {
                    b"ecdsa-sha2-nistp256" => (
                        "ecdsa-sha2-nistp256",
                        b"nistp256",
                        &signature::ECDSA_P256_SHA256_FIXED,
                        32,
                    ),
                    b"ecdsa-sha2-nistp384" => (
                        "ecdsa-sha2-nistp384",
                        b"nistp384",
                        &signature::ECDSA_P384_SHA384_FIXED,
                        48,
                    ),
                    b"ecdsa-sha2-nistp521" => (
                        "ecdsa-sha2-nistp521",
                        b"nistp521",
                        &signature::ECDSA_P521_SHA512_FIXED,
                        66,
                    ),
                    _ => return None,
                };
                if reader.string()? != curve {
                    return None;
                }
                let point = reader.string()?.to_vec();
                Self::Ecdsa {
                    name,
                    algorithm,
                    scalar_len,
                    point,
                }
            }
        };
        reader.finish()?;
        Some(key)
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Ed25519(_) => "ssh-ed25519",
            Self::Ecdsa { name, .. } => name,
            Self::Rsa { .. } => "ssh-rsa",
        }
    }

    /// Verify `blob`, a signature in `format`, over `message`.
    ///
    /// Returns `None` if the format does not fit the key or the signature is
    /// malformed.
    fn verify(
        &self,
        format: &[u8],
        blob: &[u8],
        message: &[u8],
    ) -> Option<Result<(), aws_lc_rs::error::Unspecified>> {
        match self {
            Self::Ed25519(key) => {
                (format == b"ssh-ed25519").then_some(())?;
                Some(UnparsedPublicKey::new(&signature::ED25519, key).verify(message, blob))
            }
            Self::Ecdsa {
                name,
                algorithm,
                scalar_len,
                point,
            } => {
                (format == name.as_bytes()).then_some(())?;
                // RFC 5656 section 3.1.2: mpint r, mpint s.
                let mut reader = Reader(blob);
                let r = strip_mpint(reader.string()?)?;
                let s = strip_mpint(reader.string()?)?;
                reader.finish()?;
                if r.len() > *scalar_len || s.len() > *scalar_len {
                    return None;
                }
                let mut fixed = vec![0; scalar_len * 2];
                fixed[scalar_len - r.len()..*scalar_len].copy_from_slice(r);
                fixed[scalar_len * 2 - s.len()..].copy_from_slice(s);
                Some(UnparsedPublicKey::new(*algorithm, point).verify(message, &fixed))
            }
            Self::Rsa { e, n } => {
                // sshsig forbids SHA-1 `ssh-rsa` signatures.
                let params = match format {
                    b"rsa-sha2-256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                    b"rsa-sha2-512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                    _ => return None,
                };
                Some(RsaPublicKeyComponents { n, e }.verify(params, message, blob))
            }
        }
    }
}

/// Decode the key blob of an `authorized_keys` style line:
/// `<type> <base64 blob> [comment]`.
fn parse_public_key_line(line: &str) -> Option<Vec<u8>> {
    let mut fields = line.split_ascii_whitespace();
    let key_type = fields.next()?;
    let blob = BASE64_STANDARD.decode(fields.next()?).ok()?;
    // The type in front of the blob must agree with the one inside it.
    (Reader(&blob).string()? == key_type.as_bytes()).then_some(blob)
}

struct Signature {
    public_key: Vec<u8>,
    namespace: Vec<u8>,
    reserved: Vec<u8>,
    hash_algorithm: Vec<u8>,
    signature: Vec<u8>,
}

fn parse_armored_signature(armored: &[u8]) -> Option<Signature> {
    let armored = std::str::from_utf8(armored).ok()?;
    let body = armored
        .trim()
        .strip_prefix("-----BEGIN SSH SIGNATURE-----")?
        .strip_suffix("-----END SSH SIGNATURE-----")?;
    let base64: String = body.split_ascii_whitespace().collect();
    let blob = BASE64_STANDARD.decode(base64).ok()?;

    let mut reader = Reader(&blob);
    (reader.bytes(MAGIC.len())? == MAGIC).then_some(())?;
    (reader.u32()? == 1).then_some(())?;
    let signature = Signature {
        public_key: reader.string()?.to_vec(),
        namespace: reader.string()?.to_vec(),
        reserved: reader.string()?.to_vec(),
        hash_algorithm: reader.string()?.to_vec(),
        signature: reader.string()?.to_vec(),
    };
    reader.finish()?;
    Some(signature)
}

/// Reader for the SSH wire encoding (RFC 4251 section 5).
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(len)?;
        self.0 = tail;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()?;
        self.bytes(len.try_into().ok()?)
    }

    /// Trailing data is an error.
    fn finish(self) -> Option<()> {
        self.0.is_empty().then_some(())
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(
        &u32::try_from(s.len())
            .expect("sshsig fields are small")
            .to_be_bytes(),
    );
    out.extend_from_slice(s);
}

/// Strip the leading zero of a positive mpint, rejecting negative ones.
fn strip_mpint(mpint: &[u8]) -> Option<&[u8]> {
    match mpint {
        [first, ..] if first & 0x80 != 0 => None,
        [0, rest @ ..] => Some(rest),
        _ => Some(mpint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures made with OpenSSH's `ssh-keygen -Y sign -n file`, see
    /// `tests/ssh/generate.sh`.
    const MESSAGE: &[u8] = include_bytes!("../../tests/ssh/message");

    fn verify(key: &str, signature: &[u8], message: &[u8]) -> Result<bool, FetchError> {
        let verifier = SshVerifier::new(key, signature)?;
        let mut data_verifier = verifier.data_verifier();
        // Feed in two chunks to exercise the streaming path.
        let (a, b) = message.split_at(message.len() / 2);
        data_verifier.update(&Bytes::copy_from_slice(a));
        data_verifier.update(&Bytes::copy_from_slice(b));
        Ok(data_verifier.validate())
    }

    macro_rules! fixture {
        ($name:literal) => {
            (
                include_str!(concat!("../../tests/ssh/", $name, ".pub")),
                include_bytes!(concat!("../../tests/ssh/", $name, ".sig")).as_slice(),
            )
        };
    }

    const FIXTURES: &[(&str, &[u8])] = &[
        fixture!("ed25519"),
        fixture!("ecdsa256"),
        fixture!("ecdsa384"),
        fixture!("ecdsa521"),
        fixture!("rsa"),
        fixture!("rsa-sha256"),
    ];

    #[test]
    fn valid_signatures() {
        for (key, sig) in FIXTURES {
            assert!(verify(key, sig, MESSAGE).unwrap(), "{key}");
        }
    }

    #[test]
    fn tampered_message() {
        let mut message = MESSAGE.to_vec();
        message.push(b'!');
        for (key, sig) in FIXTURES {
            assert!(!verify(key, sig, &message).unwrap(), "{key}");
        }
    }

    #[test]
    fn info() {
        let (key, sig) = fixture!("ed25519");
        let info = SshVerifier::new(key, sig).unwrap().info();
        let expected = include_str!("../../tests/ssh/ed25519.fingerprint");
        assert_eq!(info, format!("ssh-ed25519 key {}", expected.trim()));
    }

    #[test]
    fn wrong_key() {
        let (key, _) = fixture!("ed25519");
        let (_, sig) = fixture!("ecdsa256");
        assert!(SshVerifier::new(key, sig).is_err());
    }

    #[test]
    fn wrong_namespace() {
        let (key, _) = fixture!("ed25519");
        let sig = include_bytes!("../../tests/ssh/ed25519-git-namespace.sig");
        assert!(SshVerifier::new(key, sig).is_err());
    }

    #[test]
    fn mismatched_key_type() {
        let (key, sig) = fixture!("ed25519");
        let key = key.replacen("ssh-ed25519", "ssh-rsa", 1);
        assert!(SshVerifier::new(&key, sig).is_err());
    }

    #[test]
    fn malformed() {
        let (key, _) = fixture!("ed25519");
        assert!(SshVerifier::new(key, b"").is_err());
        assert!(
            SshVerifier::new(
                key,
                b"-----BEGIN SSH SIGNATURE-----\n-----END SSH SIGNATURE-----"
            )
            .is_err()
        );
        assert!(SshVerifier::new("ssh-ed25519 !!!", fixture!("ed25519").1).is_err());
    }

    #[test]
    fn mpint() {
        assert_eq!(strip_mpint(&[0, 0x80]), Some(&[0x80][..]));
        assert_eq!(strip_mpint(&[0x7f]), Some(&[0x7f][..]));
        assert_eq!(strip_mpint(&[0x80]), None);
        assert_eq!(strip_mpint(&[]), Some(&[][..]));
    }
}
