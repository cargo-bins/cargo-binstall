use std::{borrow::Cow, env, ffi::OsStr, fmt, iter, path::Path, sync::Arc};

use command_group::AsyncCommandGroup;
use compact_str::{CompactString, ToCompactString};
use either::Either;
use itertools::Itertools;
use semver::Version;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

use crate::{
    bins,
    errors::BinstallError,
    fetchers::Fetcher,
    manifests::crate_info::{CrateInfo, CrateSource},
    ops::Options,
};

pub struct ResolutionFetch {
    pub fetcher: Arc<dyn Fetcher>,
    pub new_version: Version,
    pub name: CompactString,
    pub version_req: CompactString,
    pub bin_files: Vec<bins::BinFile>,
}

pub struct ResolutionSource {
    pub name: CompactString,
    pub version: CompactString,
}

pub enum Resolution {
    Fetch(Box<ResolutionFetch>),
    InstallFromSource(ResolutionSource),
    AlreadyUpToDate,
}

impl Resolution {
    pub fn print(&self, opts: &Options) {
        match self {
            Resolution::Fetch(fetch) => {
                fetch.print(opts);
            }
            Resolution::InstallFromSource(source) => {
                source.print();
            }
            Resolution::AlreadyUpToDate => (),
        }
    }
}

impl ResolutionFetch {
    pub fn install(self, opts: &Options) -> Result<CrateInfo, BinstallError> {
        info!("Installing binaries...");
        for file in &self.bin_files {
            file.install_bin()?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &self.bin_files {
                file.install_link()?;
            }
        }

        Ok(CrateInfo {
            name: self.name,
            version_req: self.version_req,
            current_version: self.new_version,
            source: CrateSource::cratesio_registry(),
            target: self.fetcher.target().to_compact_string(),
            bins: self
                .bin_files
                .into_iter()
                .map(|bin| bin.base_name)
                .collect(),
        })
    }

    pub fn print(&self, opts: &Options) {
        let fetcher = &self.fetcher;
        let bin_files = &self.bin_files;
        let name = &self.name;
        let new_version = &self.new_version;

        debug!(
            "Found a binary install source: {} ({})",
            fetcher.source_name(),
            fetcher.target()
        );

        warn!(
            "The package {name} v{new_version} will be downloaded from {}{}",
            if fetcher.is_third_party() {
                "third-party source "
            } else {
                ""
            },
            fetcher.source_name()
        );

        info!("This will install the following binaries:");
        for file in bin_files {
            info!("  - {}", file.preview_bin());
        }

        if !opts.no_symlinks {
            info!("And create (or update) the following symlinks:");
            for file in bin_files {
                info!("  - {}", file.preview_link());
            }
        }
    }
}

impl ResolutionSource {
    pub async fn install(self, opts: Arc<Options>) -> Result<(), BinstallError> {
        let desired_targets = opts.desired_targets.get().await;
        let target = desired_targets
            .first()
            .ok_or(BinstallError::NoViableTargets)?;

        let name = &self.name;
        let version = &self.version;

        let cargo = env::var_os("CARGO")
            .map(Cow::Owned)
            .unwrap_or_else(|| Cow::Borrowed(OsStr::new("cargo")));

        debug!(
            "Running `{} install {name} --version {version} --target {target}`",
            Path::new(&cargo).display(),
        );

        let mut cmd = Command::new(cargo);

        cmd.arg("install")
            .arg(name)
            .arg("--version")
            .arg(version)
            .arg("--target")
            .arg(target)
            .kill_on_drop(true);

        if opts.quiet {
            cmd.arg("--quiet");
        }

        if opts.force {
            cmd.arg("--force");
        }

        if opts.locked {
            cmd.arg("--locked");
        }

        if !opts.dry_run {
            let mut child = opts
                .jobserver_client
                .get()
                .await?
                .configure_and_run(&mut cmd, |cmd| cmd.group_spawn())?;

            debug!("Spawned command pid={:?}", child.id());

            let status = child.wait().await?;
            if status.success() {
                info!("Cargo finished successfully");
                Ok(())
            } else {
                error!("Cargo errored! {status:?}");
                Err(BinstallError::SubProcess {
                    command: format_cmd(&cmd).to_string().into_boxed_str(),
                    status,
                })
            }
        } else {
            info!("Dry-run: running `{}`", format_cmd(&cmd));
            Ok(())
        }
    }

    pub fn print(&self) {
        warn!(
            "The package {} v{} will be installed from source (with cargo)",
            self.name, self.version
        )
    }
}

fn format_cmd(cmd: &Command) -> impl fmt::Display + '_ {
    let cmd = cmd.as_std();

    let program = Either::Left(Path::new(cmd.get_program()).display());

    let program_args = cmd
        .get_args()
        .map(OsStr::to_string_lossy)
        .map(Either::Right);

    iter::once(program).chain(program_args).format(" ")
}
