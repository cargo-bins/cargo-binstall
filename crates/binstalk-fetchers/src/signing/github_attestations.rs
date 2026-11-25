//! [GitHub artifact attestations] of release assets.
//!
//! Two kinds are accepted, both of which must name the repository hosting
//! the release:
//!
//! - the release attestation GitHub makes for every asset of an
//!   [immutable release], which also names the release tag;
//! - provenance made by a GitHub Actions workflow of the repository, e.g.
//!   with [`actions/attest`].
//!
//! Bundles are verified against the trusted roots embedded in
//! `sigstore-trust-root`, so no request is made besides the one to list the
//! attestations.
//!
//! [GitHub artifact attestations]: https://docs.github.com/en/actions/concepts/security/artifact-attestations
//! [immutable release]: https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases
//! [`actions/attest`]: https://github.com/actions/attest

use std::sync::OnceLock;

use aws_lc_rs::digest;
use binstalk_downloader::download::DataVerifier;
use binstalk_git_repo_api::gh_api_client::{GhApiClient, GhRelease};
use bytes::Bytes;
use serde_json::Value;
use sigstore_verify::{
    SubjectAltName, VerificationResult,
    trust_root::SigstoreInstance,
    types::{Bundle, Sha256Hash, SignatureContent},
};
use tracing::{debug, error, warn};

use super::sigstore_common::{sha256, verifier};
use crate::FetchError;

/// OIDC issuer of the certificates GitHub Actions workflows sign with.
const ACTIONS_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Identity of the certificates GitHub signs release attestations with.
const RELEASES_IDENTITY: &str = "https://dotcom.releases.github.com";

const RELEASE_PREDICATE_PREFIX: &str = "https://in-toto.io/attestation/release/";

pub struct GithubAttestationsVerifier {
    release: GhRelease,
    digest: OnceLock<Sha256Hash>,
}

impl GithubAttestationsVerifier {
    pub fn new(release: GhRelease) -> Self {
        Self {
            release,
            digest: OnceLock::new(),
        }
    }

    pub fn data_verifier(&self) -> Box<dyn DataVerifier + '_> {
        Box::new(DigestRecorder {
            digest: &self.digest,
            context: Some(digest::Context::new(&digest::SHA256)),
        })
    }

    /// Verify the attestations of the downloaded data, returning what
    /// attested it.
    pub async fn verify(&self, gh_api_client: &GhApiClient) -> Result<String, FetchError> {
        let digest = self
            .digest
            .get()
            .expect("the data verifier must be validated before verify()");
        let repo = &self.release.repo;

        let bundles: Vec<Value> = gh_api_client
            .get_attestations(repo, &digest.to_hex())
            .await?;
        if bundles.is_empty() {
            error!(
                "{}/{} has no GitHub attestation for this package",
                repo.owner, repo.repo
            );
            return Err(FetchError::MissingSignature);
        }

        let count = bundles.len();
        for bundle in bundles {
            match self.verify_bundle(bundle, digest) {
                Ok(attested_by) => return Ok(attested_by),
                Err(reason) => warn!("Rejected a GitHub attestation of the package: {reason}"),
            }
        }

        error!(
            "None of the {count} GitHub attestations of the package is valid and made by {}/{}",
            repo.owner, repo.repo
        );
        Err(FetchError::InvalidSignature)
    }

    fn verify_bundle(&self, bundle: Value, digest: &Sha256Hash) -> Result<String, String> {
        let bundle: Bundle =
            serde_json::from_value(bundle).map_err(|err| format!("malformed bundle: {err}"))?;

        let mut last_err = None;
        for instance in [SigstoreInstance::PublicGood, SigstoreInstance::GitHub] {
            let (verifier, policy) = verifier(instance)?;
            match verifier.verify(digest, &bundle, policy) {
                Ok(result) => return self.check_signer(instance, &result, &bundle),
                Err(err) => {
                    debug!(?instance, "bundle does not verify: {err}");
                    last_err = Some(err);
                }
            }
        }
        Err(format!(
            "invalid bundle: {}",
            last_err.expect("tried at least one instance")
        ))
    }

    /// Check that the verified `bundle` was made for this release.
    fn check_signer(
        &self,
        instance: SigstoreInstance,
        result: &VerificationResult,
        bundle: &Bundle,
    ) -> Result<String, String> {
        let GhRelease { repo, tag } = &self.release;
        let full_name = format!("{}/{}", repo.owner, repo.repo);

        let identity = result.identity().map(SubjectAltName::as_str);

        // GitHub's own release attestation.
        if instance == SigstoreInstance::GitHub && identity == Some(RELEASES_IDENTITY) {
            let statement = statement(bundle)?;
            let predicate_type = statement["predicateType"].as_str().unwrap_or_default();
            if !predicate_type.starts_with(RELEASE_PREDICATE_PREFIX) {
                return Err(format!(
                    "release attestation has unexpected predicate type {predicate_type:?}"
                ));
            }
            let predicate = &statement["predicate"];
            let attested_repo = predicate["repository"].as_str().unwrap_or_default();
            let attested_tag = predicate["tag"].as_str().unwrap_or_default();
            // Owner and repository names are case-insensitive on GitHub.
            if !attested_repo.eq_ignore_ascii_case(&full_name) || attested_tag != tag {
                return Err(format!(
                    "release attestation is for {attested_repo}@{attested_tag}, not {full_name}@{tag}"
                ));
            }
            return Ok(format!("GitHub release attestation of {full_name}@{tag}"));
        }

        // Provenance signed by a GitHub Actions workflow of the repository.
        if result.issuer() == Some(ACTIONS_ISSUER) {
            let claims = &result
                .certificate()
                .ok_or("verified bundle has no certificate")?
                .ci_claims;
            let source = claims.source_repository_uri.as_deref().unwrap_or_default();
            let expected = format!("https://github.com/{full_name}");
            if !source.eq_ignore_ascii_case(&expected) {
                return Err(format!(
                    "attestation was made for {source:?}, not {expected}"
                ));
            }
            let workflow = claims
                .build_signer_uri
                .as_deref()
                .or(identity)
                .unwrap_or(&expected);
            return Ok(format!("GitHub attestation by {workflow}"));
        }

        Err(format!(
            "attestation was signed by {}, issued by {}, neither GitHub releases nor GitHub Actions",
            identity.unwrap_or("an unknown identity"),
            result.issuer().unwrap_or("an unknown issuer"),
        ))
    }
}

/// The in-toto statement a DSSE bundle signs.
fn statement(bundle: &Bundle) -> Result<Value, String> {
    match &bundle.content {
        SignatureContent::DsseEnvelope(envelope) => {
            serde_json::from_slice(envelope.payload.as_bytes())
                .map_err(|err| format!("malformed in-toto statement: {err}"))
        }
        _ => Err("bundle has no in-toto statement".into()),
    }
}

struct DigestRecorder<'a> {
    digest: &'a OnceLock<Sha256Hash>,
    context: Option<digest::Context>,
}

impl DataVerifier for DigestRecorder<'_> {
    fn update(&mut self, data: &Bytes) {
        if let Some(context) = &mut self.context {
            context.update(data);
        }
    }

    /// The digest is checked by [`GithubAttestationsVerifier::verify`].
    fn validate(&mut self) -> bool {
        if let Some(context) = self.context.take() {
            let _ = self.digest.set(sha256(context));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use binstalk_git_repo_api::gh_api_client::GhRepo;

    use super::*;

    /// `GET /repos/cargo-bins/cargo-binstall/attestations/sha256:<digest>`
    /// of `cargo-binstall-x86_64-unknown-linux-musl.tgz` of v1.28.0: its
    /// immutable release attestation and its `actions/attest` provenance.
    const ATTESTATIONS: &str = include_str!("../../tests/github-attestations/v1.28.0.json");
    const DIGEST: &str = "dad3d904258f50b0cc5ac34e6a6e8cefbada8bf65ab3da063faa1d6b534ad28a";

    fn bundles() -> Vec<Value> {
        let response: Value = serde_json::from_str(ATTESTATIONS).unwrap();
        response["attestations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|attestation| attestation["bundle"].clone())
            .collect()
    }

    fn verifier(owner: &str, repo: &str, tag: &str) -> GithubAttestationsVerifier {
        GithubAttestationsVerifier::new(GhRelease {
            repo: GhRepo {
                owner: owner.into(),
                repo: repo.into(),
            },
            tag: tag.into(),
        })
    }

    fn digest() -> Sha256Hash {
        Sha256Hash::from_hex(DIGEST).unwrap()
    }

    #[test]
    fn release_attestation() {
        let [release, _] = bundles().try_into().unwrap();
        assert_eq!(
            verifier("cargo-bins", "Cargo-Binstall", "v1.28.0")
                .verify_bundle(release, &digest())
                .unwrap(),
            "GitHub release attestation of cargo-bins/Cargo-Binstall@v1.28.0"
        );
    }

    #[test]
    fn provenance() {
        let [_, provenance] = bundles().try_into().unwrap();
        assert_eq!(
            verifier("cargo-bins", "cargo-binstall", "v1.28.0")
                .verify_bundle(provenance, &digest())
                .unwrap(),
            "GitHub attestation by https://github.com/cargo-bins/cargo-binstall/.github/workflows/release-cli.yml@refs/heads/main"
        );
    }

    #[test]
    fn other_repo() {
        for bundle in bundles() {
            verifier("cargo-bins", "cargo-quickinstall", "v1.28.0")
                .verify_bundle(bundle, &digest())
                .unwrap_err();
        }
    }

    #[test]
    fn other_tag() {
        let [release, _] = bundles().try_into().unwrap();
        verifier("cargo-bins", "cargo-binstall", "v1.27.0")
            .verify_bundle(release, &digest())
            .unwrap_err();
    }

    #[test]
    fn other_digest() {
        let digest = Sha256Hash::new([0; 32]);
        for bundle in bundles() {
            verifier("cargo-bins", "cargo-binstall", "v1.28.0")
                .verify_bundle(bundle, &digest)
                .unwrap_err();
        }
    }
}
