//! Fetch all latest non-pre crates from crates.io.
//!
//! Downloads them from the CDN so it doesn't hit crates.io itself, uses binstall's utilities to
//! extract the tarballs in a streaming fashion, and puts everything in a ./crates/ folder.
#![cfg_attr(not(tokio_unstable), allow(warnings))]
#[cfg(not(tokio_unstable))] fn main() {}

use std::{env, fmt::Display};

use binstall::{helpers::download::Download, manifests::cargo_toml_binstall::PkgFmt};
use crates_index::Index;
use miette::{IntoDiagnostic, Result};
use rayon::prelude::ParallelIterator;
use reqwest::Client;
use semver::Version;
use sha2::Sha256;
#[cfg(tokio_unstable)]
use tokio::task::JoinSet;
use tracing::{error, info};
use tracing_subscriber::fmt::format::FmtSpan;
use url::Url;

const CHUNK_SIZE: usize = 512;

#[cfg(tokio_unstable)]
#[tokio::main]
#[tracing::instrument]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or("info".into()))
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();

    info!("Gathering all crates we need to fetch");
    let all_latests = read_index()?;
    let total = all_latests.len();
    info!(%total, "that's a lot of crates");

    let client = Client::builder()
        .http2_prior_knowledge()
        .http2_adaptive_window(true)
        .build()
        .into_diagnostic()?;

    for (n, chunk) in all_latests.chunks(CHUNK_SIZE).enumerate() {
        info!("Processing {n}th of {}", all_latests.len() / CHUNK_SIZE + 1);
        get_a_bunch(client.clone(), n, total, chunk).await?;
    }

    Ok(())
}

#[cfg(tokio_unstable)]
async fn get_a_bunch(client: Client, n: usize, total: usize, chunk: &[IndexCrate]) -> Result<()> {
    let mut set = JoinSet::new();

    for (i, crate_version) in chunk.into_iter().enumerate() {
        set.spawn(crate_version.clone().download(client.clone(), n * CHUNK_SIZE + i, total));
    }

    while let Some(res) = set.join_next().await {
        if let Err(err) = res {
            error!("{err}");
        }
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct IndexCrate {
    pub name: String,
    pub version: Version,
    pub url: Url,
    pub checksum: [u8; 32],
}

impl IndexCrate {
    #[tracing::instrument(level = "debug")]
    async fn download(self, client: Client, i: usize, total: usize) -> Result<()> {
        info!(target=%self, "downloading {i}/{total}");
        Download::<Sha256>::new_with_checksum(client, self.url.clone(), self.checksum[..].to_vec())
            .and_extract(PkgFmt::Tgz, "./crates")
            .await?;
        info!(target=%self, "downloaded {i}/{total}");

        Ok(())
    }
}

impl Display for IndexCrate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            name,
            version,
            url,
            checksum,
        } = self;
        let ch = u64::from_le_bytes(checksum[0..8].try_into().unwrap());
        let ec = u64::from_le_bytes(checksum[8..16].try_into().unwrap());
        let ks = u64::from_le_bytes(checksum[16..24].try_into().unwrap());
        let um = u64::from_le_bytes(checksum[24..32].try_into().unwrap());
        write!(f, "{name}@{version}: {url} [{ch:x} {ec:x} {ks:x} {um:x}]")
    }
}

#[tracing::instrument]
fn read_index() -> Result<Vec<IndexCrate>> {
    let index = Index::new_cargo_default().into_diagnostic()?;
    let all_latests: Vec<_> = index
        .crates_parallel()
        .filter_map(|c| {
            c.ok().and_then(|c| {
                c.highest_normal_version().map(|v| {
                    let name = v.name().into();
                    let version = v.version().parse().unwrap();
                    let url = Url::parse(&format!(
                        "https://static.crates.io/crates/{name}/{name}-{version}.crate"
                    ))
                    .unwrap();
                    IndexCrate {
                        name,
                        version,
                        url,
                        checksum: *v.checksum(),
                    }
                })
            })
        })
        .collect();
    Ok(all_latests)
}
