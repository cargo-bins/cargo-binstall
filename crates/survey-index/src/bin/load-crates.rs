//! Loads up Cargo.toml metadata into postgres.
//!
//! Uses a ./crates/ folder prepared by fetch-crates, and connects to the database at `DATABASE_URL`
//! (env variable). Parses the Cargo.toml of each crate into a JSON structure and loads it into a
//! table `crates` which should be created like:
//!
//! ```sql
//! CREATE TABLE crates (
//!     name varchar(200) not null,
//!     version varchar(100) not null,
//!     manifest jsonb not null
//! );
//! ```
//!
//! You may want to add indices to make querying easier. This tool doesn't care.
#![cfg_attr(not(tokio_unstable), allow(warnings))]
#[cfg(not(tokio_unstable))]
fn main() {}

use std::{env, fs::read_dir, path::PathBuf, sync::Arc};

use cargo_toml::Manifest;
use miette::{IntoDiagnostic, Result};
use tokio_postgres::{Client};
#[cfg(tokio_unstable)]
use tokio::task::JoinSet;
use tracing::{error, info};
use tracing_subscriber::fmt::format::FmtSpan;

const CHUNK_SIZE: usize = 512;

#[cfg(tokio_unstable)]
#[tokio::main]
async fn main() -> Result<()> {
    use std::time::Duration;

    use tokio::time::sleep;

    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or("info".into()))
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .init();

    let crates = read_dir("./crates/")
        .into_diagnostic()?
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .into_diagnostic()?;
    let total = crates.len();
    info!(%total, "listed some crates");

    let (client, conn) = tokio_postgres::connect(
        &env::var("DATABASE_URL").expect("DATABASE_URL not set"),
        tokio_postgres::tls::NoTls,
    )
    .await
    .into_diagnostic()?;
    info!("connected to postgres");

    let conntask = tokio::spawn(conn);
    let client = Arc::new(client);

    for (n, chunk) in crates.chunks(CHUNK_SIZE).enumerate() {
        info!("Processing {n}th of {}", crates.len() / CHUNK_SIZE + 1);
        load_a_bunch(client.clone(), n, total, chunk).await?;
    }

    sleep(Duration::from_secs(1)).await;
    drop(client);

    info!("waiting for postgres to finish");
    conntask.await.into_diagnostic()?.into_diagnostic()?;

    info!("done");
    Ok(())
}

#[cfg(tokio_unstable)]
async fn load_a_bunch(client: Arc<Client>, n: usize, total: usize, chunk: &[PathBuf]) -> Result<()> {
    let mut set = JoinSet::new();

    for (i, crate_path) in chunk.into_iter().enumerate() {
        set.spawn(parse_and_load_crate(crate_path.clone(), client.clone(), n * CHUNK_SIZE + i, total));
    }

    while let Some(res) = set.join_next().await {
        let res = res.into_diagnostic()?;
        if let Err(err) = res {
            error!("{err:?}");
        }
    }

    Ok(())
}

#[tracing::instrument(level = "debug")]
async fn parse_and_load_crate(path: PathBuf, client: Arc<Client>, i: usize, total: usize) -> Result<()> {
    info!("parsing {i}/{total}");
    let toml = Manifest::from_path(path.join("Cargo.toml")).into_diagnostic()?;
    let package = if let Some(ref pkg) = toml.package {
        pkg
    } else {
        miette::bail!("Not a package crate: {path}");
    };

    info!("loading {i}/{total}");
    let json = serde_json::to_value(&toml).into_diagnostic()?;
    client.execute("INSERT INTO crates (name, version, manifest) VALUES ($1, $2, $3);", &[
        &package.name,
        &package.version,
        &json,
    ]).await.into_diagnostic()?;

    info!("loaded {i}/{total}");
    Ok(())
}
