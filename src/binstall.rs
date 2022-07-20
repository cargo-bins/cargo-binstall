use std::{collections::BTreeSet, path::PathBuf, process, sync::Arc};

use cargo_toml::Package;
use log::{debug, error, info};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use tokio::{process::Command, task::block_in_place};

use super::{bins, fetchers::Fetcher, *};

mod resolve;
pub use resolve::*;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub version: Option<String>,
    pub manifest_path: Option<PathBuf>,
}

pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
    desired_targets: DesiredTargets,
    jobserver_client: jobserver::Client,
) -> Result<()> {
    match resolution {
        Resolution::Fetch {
            fetcher,
            package,
            name,
            version,
            bin_path,
            bin_files,
        } => {
            let cvs = metafiles::CrateVersionSource {
                name,
                version: package.version.parse().into_diagnostic()?,
                source: metafiles::Source::cratesio_registry(),
            };

            install_from_package(fetcher, opts, cvs, version, bin_path, bin_files).await
        }
        Resolution::InstallFromSource { package } => {
            let desired_targets = desired_targets.get().await;
            let target = desired_targets
                .first()
                .ok_or_else(|| miette!("No viable targets found, try with `--targets`"))?;

            if !opts.dry_run {
                install_from_source(package, target, jobserver_client).await
            } else {
                info!(
                    "Dry-run: running `cargo install {} --version {} --target {target}`",
                    package.name, package.version
                );
                Ok(())
            }
        }
    }
}

async fn install_from_package(
    fetcher: Arc<dyn Fetcher>,
    opts: Arc<Options>,
    cvs: metafiles::CrateVersionSource,
    version: String,
    bin_path: PathBuf,
    bin_files: Vec<bins::BinFile>,
) -> Result<()> {
    // Download package
    if opts.dry_run {
        info!("Dry run, not downloading package");
    } else {
        fetcher.fetch_and_extract(&bin_path).await?;
    }

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone()).flatten() {
            debug!("Found public key: {pub_key}");

            // Generate signature file URL
            let mut sig_ctx = ctx.clone();
            sig_ctx.format = "sig".to_string();
            let sig_url = sig_ctx.render(&pkg_url)?;

            debug!("Fetching signature file: {sig_url}");

            // Download signature file
            let sig_path = temp_dir.join(format!("{pkg_name}.sig"));
            download(&sig_url, &sig_path).await?;

            // TODO: do the signature check
            unimplemented!()
        } else {
            warn!("No public key found, package signature could not be validated");
        }
    }

    if opts.dry_run {
        info!("Dry run, not proceeding");
        return Ok(());
    }

    info!("Installing binaries...");
    block_in_place(|| {
        for file in &bin_files {
            file.install_bin()?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &bin_files {
                file.install_link()?;
            }
        }

        let bins: BTreeSet<String> = bin_files.into_iter().map(|bin| bin.base_name).collect();

        {
            debug!("Writing .crates.toml");
            let mut c1 = metafiles::v1::CratesToml::load().unwrap_or_default();
            c1.insert(cvs.clone(), bins.clone());
            c1.write()?;
        }

        {
            debug!("Writing .crates2.json");
            let mut c2 = metafiles::v2::Crates2Json::load().unwrap_or_default();
            c2.insert(
                cvs,
                metafiles::v2::CrateInfo {
                    version_req: Some(version),
                    bins,
                    profile: "release".into(),
                    target: fetcher.target().to_string(),
                    rustc: format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                    ..Default::default()
                },
            );
            c2.write()?;
        }

        Ok(())
    })
}

async fn install_from_source(
    package: Package<Meta>,
    target: &str,
    jobserver_client: jobserver::Client,
) -> Result<()> {
    debug!(
        "Running `cargo install {} --version {} --target {target}`",
        package.name, package.version
    );
    let mut command = process::Command::new("cargo");
    jobserver_client.configure(&mut command);

    let mut child = Command::from(command)
        .arg("install")
        .arg(package.name)
        .arg("--version")
        .arg(package.version)
        .arg("--target")
        .arg(&*target)
        .spawn()
        .into_diagnostic()
        .wrap_err("Spawning cargo install failed.")?;
    debug!("Spawned command pid={:?}", child.id());

    let status = child
        .wait()
        .await
        .into_diagnostic()
        .wrap_err("Running cargo install failed.")?;
    if status.success() {
        info!("Cargo finished successfully");
        Ok(())
    } else {
        error!("Cargo errored! {status:?}");
        Err(miette!("Cargo install error"))
    }
}
