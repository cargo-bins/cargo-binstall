//! What verifying [Sigstore] bundles of keyless signatures and of GitHub
//! artifact attestations have in common.
//!
//! [Sigstore]: https://www.sigstore.dev/

use std::sync::LazyLock;

use aws_lc_rs::digest;
use sigstore_verify::{
    VerificationPolicy, Verifier,
    trust_root::{SigstoreInstance, TrustedRoot},
    types::Sha256Hash,
};

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
