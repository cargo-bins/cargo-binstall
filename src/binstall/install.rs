use std::{path::PathBuf, process, sync::Arc};

use cargo_toml::Package;
use compact_str::CompactString;
use log::{debug, error, info};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use tokio::{process::Command, task::block_in_place};

use super::{MetaData, Options, Resolution};
use crate::{bins, fetchers::Fetcher, metafiles::binstall_v1::Source, *};

pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
    jobserver_client: LazyJobserverClient,
) -> Result<Option<MetaData>> {
    match resolution {
        Resolution::Fetch {
            fetcher,
            package,
            name,
            version,
            bin_path,
            bin_files,
        } => {
            let current_version = package.version.parse().into_diagnostic()?;
            let target = fetcher.target().into();

            install_from_package(fetcher, opts, bin_path, bin_files)
                .await
                .map(|option| {
                    option.map(|bins| MetaData {
                        name,
                        version_req: version,
                        current_version,
                        source: Source::cratesio_registry(),
                        target,
                        bins,
                        other: Default::default(),
                    })
                })
        }
        Resolution::InstallFromSource { package } => {
            let desired_targets = opts.desired_targets.get().await;
            let target = desired_targets
                .first()
                .ok_or_else(|| miette!("No viable targets found, try with `--targets`"))?;

            if !opts.dry_run {
                install_from_source(package, target, jobserver_client)
                    .await
                    .map(|_| None)
            } else {
                info!(
                    "Dry-run: running `cargo install {} --version {} --target {target}`",
                    package.name, package.version
                );
                Ok(None)
            }
        }
    }
}

async fn install_from_package(
    fetcher: Arc<dyn Fetcher>,
    opts: Arc<Options>,
    bin_path: PathBuf,
    bin_files: Vec<bins::BinFile>,
) -> Result<Option<Vec<CompactString>>> {
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
        return Ok(None);
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

        Ok(Some(
            bin_files.into_iter().map(|bin| bin.base_name).collect(),
        ))
    })
}

async fn install_from_source(
    package: Package<Meta>,
    target: &str,
    lazy_jobserver_client: LazyJobserverClient,
) -> Result<()> {
    let jobserver_client = lazy_jobserver_client.get().await?;

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
