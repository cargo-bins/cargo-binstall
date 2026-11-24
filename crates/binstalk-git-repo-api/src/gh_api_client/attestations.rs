use compact_str::format_compact;
use serde::{Deserialize, de::DeserializeOwned};

use super::{GhApiError, GhRepo, common::issue_restful_api, remote};

#[derive(Deserialize)]
struct Attestations<T> {
    attestations: Vec<Attestation<T>>,
}

#[derive(Deserialize)]
struct Attestation<T> {
    /// Missing for large bundles, which are only available, snappy
    /// compressed, at `bundle_url`.
    bundle: Option<T>,
}

pub(super) async fn fetch_attestations_restful_api<T: DeserializeOwned>(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    sha256: &str,
    auth_token: Option<&str>,
) -> Result<Vec<T>, GhApiError> {
    let subject_digest = format_compact!("sha256:{sha256}");
    let Attestations { attestations } = issue_restful_api(
        client,
        &["repos", owner, repo, "attestations", &subject_digest],
        auth_token,
    )
    .await?;

    Ok(attestations
        .into_iter()
        .filter_map(|attestation| attestation.bundle)
        .collect())
}
