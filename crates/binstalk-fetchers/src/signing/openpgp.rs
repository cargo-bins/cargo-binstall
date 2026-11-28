//! Detached [OpenPGP] signatures, as made by `gpg --detach-sign`.
//!
//! Only what verifying a package needs is implemented: the public key comes
//! from the crate's own `Cargo.toml`, so every key and subkey in it is trusted
//! as is. Self-signatures, binding signatures, key expiry and revocation are
//! not checked.
//!
//! Supported are v4 and v6 keys and binary-document signatures, with RSA
//! (2048 bits or more), ECDSA over NIST P-256/384/521, and Ed25519 (both the
//! legacy EdDSA and the RFC 9580 encoding), over SHA-256/384/512.
//!
//! [OpenPGP]: https://www.rfc-editor.org/rfc/rfc9580

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use aws_lc_rs::{
    digest,
    signature::{self, EcdsaVerificationAlgorithm, RsaPublicKeyComponents, UnparsedPublicKey},
};
use base64::prelude::{BASE64_STANDARD, Engine};
use binstalk_downloader::download::DataVerifier;
use bytes::Bytes;
use tracing::{error, trace};

use crate::FetchError;

mod packet;
use packet::{Packet, Reader};

const TAG_SIGNATURE: u8 = 2;
const TAG_PUBLIC_KEY: u8 = 6;
const TAG_USER_ID: u8 = 13;
const TAG_PUBLIC_SUBKEY: u8 = 14;

const ALGO_RSA: u8 = 1;
const ALGO_RSA_SIGN_ONLY: u8 = 3;
const ALGO_ECDSA: u8 = 19;
const ALGO_EDDSA_LEGACY: u8 = 22;
const ALGO_ED25519: u8 = 27;

const SIG_TYPE_BINARY: u8 = 0x00;
const SIG_TYPE_TEXT: u8 = 0x01;

const SUBPACKET_CREATION_TIME: u8 = 2;
const SUBPACKET_EXPIRATION_TIME: u8 = 3;
const SUBPACKET_ISSUER_KEY_ID: u8 = 16;
const SUBPACKET_ISSUER_FINGERPRINT: u8 = 33;

pub struct OpenPgpVerifier {
    keys: Vec<Key>,
    signatures: Vec<Signature>,
    /// The first user ID of the key, it is as trusted as the key.
    user_id: Option<String>,
}

impl OpenPgpVerifier {
    pub fn new(pubkey: &str, signature: &[u8]) -> Result<Self, FetchError> {
        trace!(key=?pubkey, "parsing OpenPGP public key");
        let (keys, user_id) = parse_keys(pubkey).map_err(|err| {
            error!("Package public key is invalid: {err}");
            FetchError::InvalidSignature
        })?;

        trace!(?signature, "parsing OpenPGP signature");
        let signatures = parse_signatures(signature).map_err(|err| {
            error!("Signature file is invalid: {err}");
            FetchError::InvalidSignature
        })?;

        // Only keep the signatures one of the keys could have made, so that
        // nothing is hashed for the others.
        let signatures: Vec<_> = signatures
            .into_iter()
            .filter(|signature| keys.iter().any(|key| signature.may_be_made_by(key)))
            .collect();
        if signatures.is_empty() {
            error!(
                "Signature was not made by a supported key of the package's public key {}",
                hex_upper(&keys[0].fingerprint)
            );
            return Err(FetchError::InvalidSignature);
        }

        Ok(Self {
            keys,
            signatures,
            user_id,
        })
    }

    pub fn data_verifier(&self) -> Box<dyn DataVerifier + '_> {
        Box::new(OpenPgpDataVerifier {
            verifier: self,
            contexts: self
                .signatures
                .iter()
                .map(|signature| {
                    let mut context = digest::Context::new(signature.hash.algorithm());
                    // v6 signatures hash their salt first.
                    context.update(&signature.salt);
                    Some(context)
                })
                .collect(),
        })
    }

    pub fn info(&self) -> String {
        let primary = hex_upper(&self.keys[0].fingerprint);
        match &self.user_id {
            Some(user_id) => format!("OpenPGP key {primary} ({user_id})"),
            None => format!("OpenPGP key {primary}"),
        }
    }
}

struct OpenPgpDataVerifier<'a> {
    verifier: &'a OpenPgpVerifier,
    /// One per signature.
    contexts: Vec<Option<digest::Context>>,
}

impl DataVerifier for OpenPgpDataVerifier<'_> {
    fn update(&mut self, data: &Bytes) {
        for context in self.contexts.iter_mut().flatten() {
            context.update(data);
        }
    }

    fn validate(&mut self) -> bool {
        for (signature, context) in self.verifier.signatures.iter().zip(&mut self.contexts) {
            let Some(mut context) = context.take() else {
                continue;
            };
            context.update(&signature.trailer);
            let digest = context.finish();
            if digest.as_ref()[..2] != signature.left16 {
                continue;
            }
            if self
                .verifier
                .keys
                .iter()
                .any(|key| signature.may_be_made_by(key) && signature.verify(key, &digest))
            {
                return true;
            }
        }

        error!("OpenPGP signature does not match the package");
        false
    }
}

#[derive(Clone, Copy, Debug)]
enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn from_id(id: u8) -> Result<Self, String> {
        match id {
            8 => Ok(Self::Sha256),
            9 => Ok(Self::Sha384),
            10 => Ok(Self::Sha512),
            1 => Err("MD5 is not accepted".into()),
            2 => Err("SHA-1 is not accepted".into()),
            id => Err(format!("hash algorithm {id} is not supported")),
        }
    }

    fn algorithm(self) -> &'static digest::Algorithm {
        match self {
            Self::Sha256 => &digest::SHA256,
            Self::Sha384 => &digest::SHA384,
            Self::Sha512 => &digest::SHA512,
        }
    }

    /// RFC 9580 section 9.5: the salt size of v6 signatures.
    fn salt_len(self) -> usize {
        match self {
            Self::Sha256 => 16,
            Self::Sha384 => 24,
            Self::Sha512 => 32,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Curve {
    P256,
    P384,
    P521,
}

impl Curve {
    fn from_oid(oid: &[u8]) -> Option<Self> {
        match oid {
            [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07] => Some(Self::P256),
            [0x2B, 0x81, 0x04, 0x00, 0x22] => Some(Self::P384),
            [0x2B, 0x81, 0x04, 0x00, 0x23] => Some(Self::P521),
            _ => None,
        }
    }

    fn algorithm(self, hash: Hash) -> &'static EcdsaVerificationAlgorithm {
        use signature::*;
        match (self, hash) {
            (Self::P256, Hash::Sha256) => &ECDSA_P256_SHA256_ASN1,
            (Self::P256, Hash::Sha384) => &ECDSA_P256_SHA384_ASN1,
            (Self::P256, Hash::Sha512) => &ECDSA_P256_SHA512_ASN1,
            (Self::P384, Hash::Sha256) => &ECDSA_P384_SHA256_ASN1,
            (Self::P384, Hash::Sha384) => &ECDSA_P384_SHA384_ASN1,
            (Self::P384, Hash::Sha512) => &ECDSA_P384_SHA512_ASN1,
            (Self::P521, Hash::Sha256) => &ECDSA_P521_SHA256_ASN1,
            (Self::P521, Hash::Sha384) => &ECDSA_P521_SHA384_ASN1,
            (Self::P521, Hash::Sha512) => &ECDSA_P521_SHA512_ASN1,
        }
    }
}

const ED25519_OID: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01];

#[derive(Debug)]
enum KeyMaterial {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ecdsa { curve: Curve, point: Vec<u8> },
    Ed25519(Vec<u8>),
}

#[derive(Debug)]
struct Key {
    version: u8,
    algorithm: u8,
    fingerprint: Vec<u8>,
    material: KeyMaterial,
}

impl Key {
    /// The 64-bit key ID: the low bits of a v4 fingerprint, the high bits
    /// of a v6 one.
    fn key_id(&self) -> &[u8] {
        if self.version == 4 {
            &self.fingerprint[self.fingerprint.len() - 8..]
        } else {
            &self.fingerprint[..8]
        }
    }
}

/// RFC 9580 section 5.5.2: parse a public key or subkey packet, `None` if
/// its version or algorithm isn't supported.
fn parse_key(body: &[u8]) -> Result<Option<Key>, String> {
    let mut reader = Reader(body);
    let version = reader.u8()?;
    let (fingerprint, algorithm, material) = match version {
        4 => {
            let mut context = digest::Context::new(&digest::SHA1_FOR_LEGACY_USE_ONLY);
            context.update(&[0x99]);
            context.update(
                &u16::try_from(body.len())
                    .map_err(|_| "key too large")?
                    .to_be_bytes(),
            );
            context.update(body);
            let _created = reader.bytes(4)?;
            let algorithm = reader.u8()?;
            (context.finish(), algorithm, reader.rest())
        }
        6 => {
            let mut context = digest::Context::new(&digest::SHA256);
            context.update(&[0x9B]);
            context.update(
                &u32::try_from(body.len())
                    .map_err(|_| "key too large")?
                    .to_be_bytes(),
            );
            context.update(body);
            let _created = reader.bytes(4)?;
            let algorithm = reader.u8()?;
            let len = reader.u32()?;
            let material = reader.bytes(len.try_into().map_err(|_| "key too large")?)?;
            reader.finish()?;
            (context.finish(), algorithm, material)
        }
        _ => return Ok(None),
    };

    let mut reader = Reader(material);
    let material = match algorithm {
        ALGO_RSA | ALGO_RSA_SIGN_ONLY => {
            let n = reader.mpi()?.to_vec();
            let e = reader.mpi()?.to_vec();
            KeyMaterial::Rsa { n, e }
        }
        ALGO_ECDSA => {
            let oid_len = reader.u8()?;
            let Some(curve) = Curve::from_oid(reader.bytes(oid_len.into())?) else {
                return Ok(None);
            };
            KeyMaterial::Ecdsa {
                curve,
                point: reader.mpi()?.to_vec(),
            }
        }
        ALGO_EDDSA_LEGACY => {
            let oid_len = reader.u8()?;
            if reader.bytes(oid_len.into())? != ED25519_OID {
                return Ok(None);
            }
            // Native point format: 0x40 prefix.
            match reader.mpi()? {
                [0x40, point @ ..] if point.len() == 32 => KeyMaterial::Ed25519(point.to_vec()),
                _ => return Err("invalid Ed25519 point".into()),
            }
        }
        ALGO_ED25519 => KeyMaterial::Ed25519(reader.bytes(32)?.to_vec()),
        _ => return Ok(None),
    };
    // v4 key material is not length-prefixed, so trailing data can't be
    // told apart from it.
    if version == 6 {
        reader.finish()?;
    }

    Ok(Some(Key {
        version,
        algorithm,
        fingerprint: fingerprint.as_ref().to_vec(),
        material,
    }))
}

/// Parse the armored transferable public key, returning its usable keys and
/// first user ID.
fn parse_keys(armored: &str) -> Result<(Vec<Key>, Option<String>), String> {
    let data = dearmor(armored.as_bytes(), "PGP PUBLIC KEY BLOCK")?
        .ok_or("expected an armored PGP PUBLIC KEY BLOCK")?;

    let mut keys = Vec::new();
    let mut user_id = None;
    let mut primary_seen = false;
    for packet in packet::Packets(&data) {
        let Packet { tag, body } = packet?;
        match tag {
            TAG_PUBLIC_KEY if primary_seen => {
                return Err("expected one key, found several".into());
            }
            TAG_PUBLIC_KEY | TAG_PUBLIC_SUBKEY => {
                if tag == TAG_PUBLIC_KEY {
                    primary_seen = true;
                } else if !primary_seen {
                    return Err("subkey before the primary key".into());
                }
                keys.extend(parse_key(body)?);
            }
            TAG_USER_ID if user_id.is_none() => {
                user_id = Some(String::from_utf8_lossy(body).into_owned());
            }
            _ => (),
        }
    }

    if !primary_seen {
        return Err("no public key found".into());
    }
    if keys.is_empty() {
        return Err("no key or subkey uses a supported version and algorithm".into());
    }
    Ok((keys, user_id))
}

#[derive(Debug)]
enum Issuer {
    Fingerprint(Vec<u8>),
    KeyId([u8; 8]),
    Unknown,
}

#[derive(Debug)]
struct Signature {
    algorithm: u8,
    hash: Hash,
    /// Empty for v4 signatures.
    salt: Vec<u8>,
    /// Hashed after the data.
    trailer: Vec<u8>,
    left16: [u8; 2],
    issuer: Issuer,
    /// The algorithm-specific fields.
    material: Vec<u8>,
}

impl Signature {
    fn may_be_made_by(&self, key: &Key) -> bool {
        self.algorithm == key.algorithm
            && match &self.issuer {
                Issuer::Fingerprint(fingerprint) => *fingerprint == key.fingerprint,
                Issuer::KeyId(key_id) => key_id == key.key_id(),
                Issuer::Unknown => true,
            }
    }

    fn verify(&self, key: &Key, digest: &digest::Digest) -> bool {
        self.try_verify(key, digest).is_some()
    }

    fn try_verify(&self, key: &Key, digest: &digest::Digest) -> Option<()> {
        let mut reader = Reader(&self.material);
        match &key.material {
            KeyMaterial::Rsa { n, e } => {
                let params = match self.hash {
                    Hash::Sha256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                    Hash::Sha384 => &signature::RSA_PKCS1_2048_8192_SHA384,
                    Hash::Sha512 => &signature::RSA_PKCS1_2048_8192_SHA512,
                };
                let sig = reader.mpi().ok()?;
                // The MPI drops leading zeros, but the signature must be as
                // long as the modulus.
                let sig = left_pad(sig, n.len())?;
                RsaPublicKeyComponents { n, e }
                    .to_parsed_public_key(params)
                    .ok()?
                    .verify_digest_sig(digest, &sig)
                    .ok()
            }
            KeyMaterial::Ecdsa { curve, point } => {
                let r = reader.mpi().ok()?;
                let s = reader.mpi().ok()?;
                UnparsedPublicKey::new(curve.algorithm(self.hash), point)
                    .verify_digest(digest, &der_signature(r, s))
                    .ok()
            }
            KeyMaterial::Ed25519(point) => {
                let sig = if self.algorithm == ALGO_ED25519 {
                    reader.bytes(64).ok()?.to_vec()
                } else {
                    let mut sig = left_pad(reader.mpi().ok()?, 32)?;
                    sig.extend(left_pad(reader.mpi().ok()?, 32)?);
                    sig
                };
                // EdDSA in OpenPGP signs the digest.
                UnparsedPublicKey::new(&signature::ED25519, point)
                    .verify(digest.as_ref(), &sig)
                    .ok()
            }
        }
    }
}

fn parse_signatures(data: &[u8]) -> Result<Vec<Signature>, String> {
    let dearmored = dearmor(data, "PGP SIGNATURE")?;
    let data = dearmored.as_deref().unwrap_or(data);

    let now = SystemTime::now();
    let mut signatures = Vec::new();
    let mut last_err = None;
    for packet in packet::Packets(data) {
        let Packet { tag, body } = packet?;
        if tag != TAG_SIGNATURE {
            continue;
        }
        match parse_signature(body, now) {
            Ok(signature) => signatures.push(signature),
            Err(err) => last_err = Some(err),
        }
    }

    if signatures.is_empty() {
        Err(last_err.unwrap_or_else(|| "no signature found".into()))
    } else {
        Ok(signatures)
    }
}

/// RFC 9580 section 5.2.3: parse a v4 or v6 signature packet.
fn parse_signature(body: &[u8], now: SystemTime) -> Result<Signature, String> {
    let mut reader = Reader(body);
    let version = reader.u8()?;
    if version != 4 && version != 6 {
        return Err(format!("signature version {version} is not supported"));
    }
    match reader.u8()? {
        SIG_TYPE_BINARY => (),
        SIG_TYPE_TEXT => {
            return Err("text signatures (`gpg --textmode`) are not supported".into());
        }
        sig_type => {
            return Err(format!(
                "signature type {sig_type:#04x} is not a document signature"
            ));
        }
    }
    let algorithm = reader.u8()?;
    let hash = Hash::from_id(reader.u8()?)?;

    let subpackets_len = |reader: &mut Reader<'_>| -> Result<usize, String> {
        if version == 4 {
            Ok(reader.u16()?.into())
        } else {
            reader
                .u32()?
                .try_into()
                .map_err(|_| "subpackets too large".into())
        }
    };
    let hashed_len = subpackets_len(&mut reader)?;
    let hashed = reader.bytes(hashed_len)?;
    let hashed_end = body.len() - reader.0.len();
    let unhashed_len = subpackets_len(&mut reader)?;
    let unhashed = reader.bytes(unhashed_len)?;
    let left16 = reader.bytes(2)?.try_into().expect("2 bytes were read");
    let salt = if version == 6 {
        let len = reader.u8()?;
        if usize::from(len) != hash.salt_len() {
            return Err("invalid salt size".into());
        }
        reader.bytes(len.into())?.to_vec()
    } else {
        Vec::new()
    };
    let material = reader.rest().to_vec();

    let mut trailer = body[..hashed_end].to_vec();
    trailer.extend_from_slice(&[version, 0xFF]);
    trailer.extend_from_slice(
        &u32::try_from(hashed_end)
            .map_err(|_| "too large")?
            .to_be_bytes(),
    );

    let mut issuer = Issuer::Unknown;
    let mut created = None;
    let mut expires_after = None;
    for (subpackets, is_hashed) in [(hashed, true), (unhashed, false)] {
        for subpacket in packet::Subpackets(subpackets) {
            let (kind, critical, data) = subpacket?;
            match kind {
                SUBPACKET_ISSUER_FINGERPRINT => match data {
                    [4, fingerprint @ ..] if fingerprint.len() == 20 => {
                        issuer = Issuer::Fingerprint(fingerprint.to_vec());
                    }
                    [6, fingerprint @ ..] if fingerprint.len() == 32 => {
                        issuer = Issuer::Fingerprint(fingerprint.to_vec());
                    }
                    _ => (),
                },
                SUBPACKET_ISSUER_KEY_ID => {
                    if let (Issuer::Unknown, Ok(key_id)) = (&issuer, data.try_into()) {
                        issuer = Issuer::KeyId(key_id);
                    }
                }
                SUBPACKET_CREATION_TIME if is_hashed => {
                    created = Some(Reader(data).u32()?);
                }
                SUBPACKET_EXPIRATION_TIME if is_hashed => {
                    expires_after = Some(Reader(data).u32()?);
                }
                kind if critical && is_hashed => {
                    return Err(format!("unsupported critical subpacket {kind}"));
                }
                _ => (),
            }
        }
    }

    // RFC 9580 section 5.2.3.18: zero means it never expires.
    if let (Some(created), Some(expires_after @ 1..)) = (created, expires_after) {
        let expiry =
            UNIX_EPOCH + Duration::from_secs(u64::from(created) + u64::from(expires_after));
        if expiry < now {
            return Err("signature has expired".into());
        }
    }

    Ok(Signature {
        algorithm,
        hash,
        salt,
        trailer,
        left16,
        issuer,
        material,
    })
}

/// RFC 9580 section 6: decode the armored `kind` block in `data`, `None` if
/// `data` isn't armored.
fn dearmor(data: &[u8], kind: &str) -> Result<Option<Vec<u8>>, String> {
    let Ok(text) = std::str::from_utf8(data) else {
        return Ok(None);
    };
    let begin = format!("-----BEGIN {kind}-----");
    let Some((_, rest)) = text.split_once(&begin) else {
        return Ok(None);
    };
    let end = format!("-----END {kind}-----");
    let (body, _) = rest
        .split_once(&end)
        .ok_or_else(|| format!("missing {end}"))?;

    let mut lines = body.lines().map(str::trim);
    // Skip the rest of the BEGIN line, and the armor headers before the
    // first blank line.
    lines.next();
    let lines = lines.skip_while(|line| !line.is_empty());
    let mut base64 = String::new();
    for line in lines {
        // The CRC-24 line, it's optional and the signature already covers the
        // data.
        if line.starts_with('=') {
            break;
        }
        base64.push_str(line);
    }
    BASE64_STANDARD
        .decode(base64)
        .map(Some)
        .map_err(|err| format!("invalid armor: {err}"))
}

/// Left-pad `bytes` with zeros to `len` bytes.
fn left_pad(bytes: &[u8], len: usize) -> Option<Vec<u8>> {
    let pad = len.checked_sub(bytes.len())?;
    let mut padded = vec![0; pad];
    padded.extend_from_slice(bytes);
    Some(padded)
}

/// DER-encode an ECDSA signature: `SEQUENCE { INTEGER r, INTEGER s }`.
fn der_signature(r: &[u8], s: &[u8]) -> Vec<u8> {
    fn integer(out: &mut Vec<u8>, mut value: &[u8]) {
        while let [0, rest @ ..] = value {
            value = rest;
        }
        let pad = value.first().is_none_or(|first| first & 0x80 != 0);
        out.push(0x02);
        der_len(out, value.len() + usize::from(pad));
        if pad {
            out.push(0);
        }
        out.extend_from_slice(value);
    }
    fn der_len(out: &mut Vec<u8>, len: usize) {
        if len < 0x80 {
            out.push(len as u8);
        } else {
            // Scalars are at most 66 bytes, so the sequence fits in 255.
            out.extend_from_slice(&[0x81, len as u8]);
        }
    }

    let mut integers = Vec::with_capacity(r.len() + s.len() + 6);
    integer(&mut integers, r);
    integer(&mut integers, s);
    let mut out = vec![0x30];
    der_len(&mut out, integers.len());
    out.extend(integers);
    out
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}");
        s
    })
}

#[cfg(test)]
mod tests;
