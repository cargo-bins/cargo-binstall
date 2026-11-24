//! [Sigstore] bundles, as made by `cosign sign-blob --bundle` or
//! `cosign attest-blob --bundle`, of keyless signatures.
//!
//! The signing certificate must be issued to the `identity` and by the OIDC
//! `issuer` of the crate's signing section. Bundles are verified against the
//! trusted roots embedded in `sigstore-trust-root`: no request is made.
//!
//! [Sigstore]: https://www.sigstore.dev/

use std::sync::LazyLock;

use aws_lc_rs::digest;
use binstalk_downloader::download::DataVerifier;
use bytes::Bytes;
use sigstore_verify::{
    SubjectAltName, VerificationPolicy, Verifier,
    trust_root::{SigstoreInstance, TrustedRoot},
    types::{Bundle, Sha256Hash},
};
use tracing::{debug, error, trace};

use crate::FetchError;

pub struct SigstoreVerifier {
    bundle: Bundle,
    identity: String,
    issuer: String,
}

impl SigstoreVerifier {
    pub fn new(bundle: &[u8], identity: String, issuer: String) -> Result<Self, FetchError> {
        trace!(?bundle, "parsing sigstore bundle");
        let bundle = serde_json::from_slice(bundle).map_err(|err| {
            error!("Signature file is not a valid Sigstore bundle: {err}");
            FetchError::InvalidSignature
        })?;
        Ok(Self {
            bundle,
            identity,
            issuer,
        })
    }

    pub fn data_verifier(&self) -> Box<dyn DataVerifier + '_> {
        Box::new(SigstoreDataVerifier {
            verifier: self,
            context: Some(digest::Context::new(&digest::SHA256)),
        })
    }

    pub fn info(&self) -> String {
        format!("Sigstore signature by {} ({})", self.identity, self.issuer)
    }

    fn verify(&self, digest: &Sha256Hash) -> bool {
        let mut last_err = None;
        for instance in [SigstoreInstance::PublicGood, SigstoreInstance::GitHub] {
            let (verifier, policy) = match verifier(instance) {
                Ok(verifier) => verifier,
                Err(err) => {
                    error!("{err}");
                    return false;
                }
            };
            let result = match verifier.verify(digest, &self.bundle, policy) {
                Ok(result) => result,
                Err(err) => {
                    debug!(?instance, "bundle does not verify: {err}");
                    last_err = Some(err);
                    continue;
                }
            };

            let identity = result.identity().map(SubjectAltName::as_str);
            let issuer = result.issuer();
            if identity == Some(&self.identity) && issuer == Some(&self.issuer) {
                return true;
            }
            error!(
                "Sigstore signature was made by {} ({}), not by {} ({})",
                identity.unwrap_or("an unknown identity"),
                issuer.unwrap_or("an unknown issuer"),
                self.identity,
                self.issuer,
            );
            return false;
        }

        error!(
            "Sigstore bundle does not match the package: {}",
            last_err.expect("tried at least one instance")
        );
        false
    }
}

struct SigstoreDataVerifier<'a> {
    verifier: &'a SigstoreVerifier,
    context: Option<digest::Context>,
}

impl DataVerifier for SigstoreDataVerifier<'_> {
    fn update(&mut self, data: &Bytes) {
        if let Some(context) = &mut self.context {
            context.update(data);
        }
    }

    fn validate(&mut self) -> bool {
        self.context
            .take()
            .is_some_and(|context| self.verifier.verify(&sha256(context)))
    }
}

pub(super) fn sha256(context: digest::Context) -> Sha256Hash {
    let digest = context.finish();
    Sha256Hash::new(
        digest
            .as_ref()
            .try_into()
            .expect("SHA-256 digests are 32 bytes"),
    )
}

/// The verifier and policy for bundles of `instance`, built once.
pub(super) fn verifier(
    instance: SigstoreInstance,
) -> Result<&'static (Verifier, VerificationPolicy), String> {
    static PUBLIC_GOOD: LazyLock<Result<(Verifier, VerificationPolicy), String>> =
        LazyLock::new(|| {
            // Who signed is checked by the callers.
            build(
                SigstoreInstance::PublicGood,
                VerificationPolicy::any_identity(),
            )
        });
    static GITHUB: LazyLock<Result<(Verifier, VerificationPolicy), String>> = LazyLock::new(|| {
        // GitHub's instance has neither a transparency log nor certificate
        // transparency: its bundles carry RFC 3161 timestamps instead, which
        // are still verified, as `gh attestation verify` does.
        build(
            SigstoreInstance::GitHub,
            VerificationPolicy::any_identity()
                .skip_tlog_unsafe()
                .skip_sct(),
        )
    });

    fn build(
        instance: SigstoreInstance,
        policy: VerificationPolicy,
    ) -> Result<(Verifier, VerificationPolicy), String> {
        TrustedRoot::from_embedded(instance)
            .map_err(|err| err.to_string())
            .and_then(|root| Verifier::new(&root).map_err(|err| err.to_string()))
            .map(|verifier| (verifier, policy))
            .map_err(|err| format!("failed to load the {instance:?} trusted root: {err}"))
    }

    match instance {
        SigstoreInstance::GitHub => &*GITHUB,
        _ => &*PUBLIC_GOOD,
    }
    .as_ref()
    .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cosign sign-blob --bundle` of `cosign-v3-blob.txt`, from
    /// sigstore-rust's test data.
    const BUNDLE: &[u8] = include_bytes!("../../tests/sigstore/cosign-v3-blob.sigstore.json");
    const BLOB: &[u8] = include_bytes!("../../tests/sigstore/cosign-v3-blob.txt");
    const IDENTITY: &str = "w.vollprecht@gmail.com";
    const ISSUER: &str = "https://github.com/login/oauth";

    /// `actions/attest` provenance of cargo-binstall v1.28.0's
    /// `x86_64-unknown-linux-musl.tgz`, as `cosign attest-blob` would make.
    fn provenance() -> Vec<u8> {
        let response: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/github-attestations/v1.28.0.json"))
                .unwrap();
        serde_json::to_vec(&response["attestations"][1]["bundle"]).unwrap()
    }

    fn verify(bundle: &[u8], identity: &str, issuer: &str, data: &[u8]) -> bool {
        let verifier = SigstoreVerifier::new(bundle, identity.into(), issuer.into()).unwrap();
        let mut data_verifier = verifier.data_verifier();
        let (a, b) = data.split_at(data.len() / 2);
        data_verifier.update(&Bytes::copy_from_slice(a));
        data_verifier.update(&Bytes::copy_from_slice(b));
        data_verifier.validate()
    }

    #[test]
    fn message_signature() {
        assert!(verify(BUNDLE, IDENTITY, ISSUER, BLOB));
        assert!(!verify(BUNDLE, IDENTITY, ISSUER, b"tampered"));
        assert!(!verify(BUNDLE, "someone@example.com", ISSUER, BLOB));
        assert!(!verify(
            BUNDLE,
            IDENTITY,
            "https://accounts.google.com",
            BLOB
        ));
    }

    #[test]
    fn dsse() {
        let identity = "https://github.com/cargo-bins/cargo-binstall/.github/workflows/release-cli.yml@refs/heads/main";
        let issuer = "https://token.actions.githubusercontent.com";
        let bundle = provenance();
        // The digest is all that's checked, not the bytes behind it.
        let verifier = SigstoreVerifier::new(&bundle, identity.into(), issuer.into()).unwrap();
        let digest = Sha256Hash::from_hex(
            "dad3d904258f50b0cc5ac34e6a6e8cefbada8bf65ab3da063faa1d6b534ad28a",
        )
        .unwrap();
        assert!(verifier.verify(&digest));
        assert!(!verifier.verify(&Sha256Hash::new([0; 32])));
        assert!(!verify(&bundle, identity, issuer, b"tampered"));
    }

    #[test]
    fn malformed() {
        assert!(SigstoreVerifier::new(b"{}", IDENTITY.into(), ISSUER.into()).is_err());
        assert!(SigstoreVerifier::new(b"", IDENTITY.into(), ISSUER.into()).is_err());
    }
}
