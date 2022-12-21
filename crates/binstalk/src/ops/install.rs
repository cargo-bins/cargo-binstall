use std::{borrow::Cow, env, ffi::OsStr, sync::Arc};

use tokio::{process::Command, task::block_in_place};
use tracing::{debug, error, info, instrument};

use super::{
    resolve::{Resolution, ResolutionInstallFromSource},
    Options,
};
use crate::{
    errors::BinstallError, helpers::jobserver_client::LazyJobserverClient,
    manifests::crate_info::CrateInfo,
};

#[instrument(skip_all)]
pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
) -> Result<Option<CrateInfo>, BinstallError> {
    match resolution {
        Resolution::AlreadyUpToDate => Ok(None),
        Resolution::Fetch(resolution_fetch) => block_in_place(|| resolution_fetch.install(&opts)),
        Resolution::InstallFromSource(ResolutionInstallFromSource { name, version }) => {
            let desired_targets = opts.desired_targets.get().await;
            let target = desired_targets
                .first()
                .ok_or(BinstallError::NoViableTargets)?;

            if !opts.dry_run {
                install_from_source(
                    &name,
                    &version,
                    target,
                    &opts.jobserver_client,
                    opts.quiet,
                    opts.force,
                )
                .await
                .map(|_| None)
            } else {
                info!(
                    "Dry-run: running `cargo install {name} --version {version} --target {target}`",
                );
                Ok(None)
            }
        }
    }
}

async fn install_from_source(
    name: &str,
    version: &str,
    target: &str,
    lazy_jobserver_client: &LazyJobserverClient,
    quiet: bool,
    force: bool,
) -> Result<(), BinstallError> {
    let jobserver_client = lazy_jobserver_client.get().await?;

    let cargo = env::var_os("CARGO")
        .map(Cow::Owned)
        .unwrap_or_else(|| Cow::Borrowed(OsStr::new("cargo")));

    debug!(
        "Running `{} install {name} --version {version} --target {target}`",
        cargo.to_string_lossy(),
    );

    let mut cmd = Command::new(cargo);

    cmd.arg("install")
        .arg(name)
        .arg("--version")
        .arg(version)
        .arg("--target")
        .arg(target)
        .kill_on_drop(true);

    if quiet {
        cmd.arg("--quiet");
    }

    if force {
        cmd.arg("--force");
    }

    let mut child = jobserver_client.configure_and_run(&mut cmd, |cmd| cmd.spawn())?;

    debug!("Spawned command pid={:?}", child.id());

    let status = child.wait().await?;
    if status.success() {
        info!("Cargo finished successfully");
        Ok(())
    } else {
        error!("Cargo errored! {status:?}");
        Err(BinstallError::SubProcess {
            command: format!("{cmd:?}").into_boxed_str(),
            status,
        })
    }
}
