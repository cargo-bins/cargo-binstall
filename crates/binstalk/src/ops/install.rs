use std::{borrow::Cow, env, ffi::OsStr, sync::Arc};

use cargo_toml::Package;
use compact_str::CompactString;
use log::{debug, error, info};
use tokio::{process::Command, task::block_in_place};

use super::{resolve::Resolution, Options};
use crate::{
    bins,
    errors::BinstallError,
    helpers::jobserver_client::LazyJobserverClient,
    manifests::{
        cargo_toml_binstall::Meta,
        crate_info::{CrateInfo, CrateSource},
    },
};

pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
    jobserver_client: LazyJobserverClient,
) -> Result<Option<CrateInfo>, BinstallError> {
    match resolution {
        Resolution::AlreadyUpToDate => Ok(None),
        Resolution::Fetch {
            fetcher,
            package,
            name,
            version_req,
            bin_files,
        } => {
            let current_version =
                package
                    .version()
                    .parse()
                    .map_err(|err| BinstallError::VersionParse {
                        v: package.version().to_string(),
                        err,
                    })?;
            let target = fetcher.target().into();

            install_from_package(opts, bin_files).await.map(|option| {
                option.map(|bins| CrateInfo {
                    name,
                    version_req,
                    current_version,
                    source: CrateSource::cratesio_registry(),
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
                .ok_or(BinstallError::NoViableTargets)?;

            if !opts.dry_run {
                install_from_source(&package, target, jobserver_client, opts.quiet, opts.force)
                    .await
                    .map(|_| None)
            } else {
                info!(
                    "Dry-run: running `cargo install {} --version {} --target {target}`",
                    package.name,
                    package.version()
                );
                Ok(None)
            }
        }
    }
}

async fn install_from_package(
    opts: Arc<Options>,
    bin_files: Vec<bins::BinFile>,
) -> Result<Option<Vec<CompactString>>, BinstallError> {
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
    package: &Package<Meta>,
    target: &str,
    lazy_jobserver_client: LazyJobserverClient,
    quiet: bool,
    force: bool,
) -> Result<(), BinstallError> {
    let jobserver_client = lazy_jobserver_client.get().await?;

    let cargo = env::var_os("CARGO")
        .map(Cow::Owned)
        .unwrap_or_else(|| Cow::Borrowed(OsStr::new("cargo")));

    debug!(
        "Running `{} install {} --version {} --target {target}`",
        cargo.to_string_lossy(),
        package.name,
        package.version()
    );

    let mut cmd = Command::new(cargo);

    cmd.arg("install")
        .arg(&package.name)
        .arg("--version")
        .arg(&package.version())
        .arg("--target")
        .arg(target);

    if quiet {
        cmd.arg("--quiet");
    }

    if force {
        cmd.arg("--force");
    }

    let command_string = format!("{:?}", cmd);

    let mut child = jobserver_client.configure_and_run(&mut cmd, |cmd| cmd.spawn())?;

    debug!("Spawned command pid={:?}", child.id());

    let status = child.wait().await?;
    if status.success() {
        info!("Cargo finished successfully");
        Ok(())
    } else {
        error!("Cargo errored! {status:?}");
        Err(BinstallError::SubProcess {
            command: command_string,
            status,
        })
    }
}
