use std::{borrow::Cow, env, ffi::OsStr, fmt, iter, path::Path, sync::Arc};

use binstalk_bins::BinFile;
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
    pub source: CrateSource,
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
        let crate_name = self.name.clone();
        self.install_inner(opts)
            .map_err(|err| err.crate_context(crate_name))
    }

    fn install_inner(self, opts: &Options) -> Result<CrateInfo, BinstallError> {
        type InstallFp = fn(&bins::BinFile) -> Result<(), bins::Error>;

        let (install_bin, install_link): (InstallFp, InstallFp) = match (opts.no_track, opts.force)
        {
            (true, true) | (false, _) => (bins::BinFile::install_bin, bins::BinFile::install_link),
            (true, false) => (
                bins::BinFile::install_bin_noclobber,
                bins::BinFile::install_link_noclobber,
            ),
        };

        info!("Installing binaries...");
        for file in &self.bin_files {
            install_bin(file)?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &self.bin_files {
                install_link(file)?;
            }
        }

        Ok(CrateInfo {
            name: self.name,
            version_req: self.version_req,
            current_version: self.new_version,
            source: self.source,
            target: self.fetcher.target().to_compact_string(),
            bins: Self::resolve_bins(&opts.bins, self.bin_files),
        })
    }

    fn resolve_bins(
        user_specified_bins: &Option<Vec<CompactString>>,
        crate_bin_files: Vec<BinFile>,
    ) -> Vec<CompactString> {
        // We need to filter crate_bin_files by user_specified_bins in case the prebuilt doesn't
        // have featured-gated (optional) binary (gated behind feature).
        crate_bin_files
            .into_iter()
            .map(|bin| bin.base_name)
            .filter(|bin_name| {
                user_specified_bins
                    .as_ref()
                    .map_or(true, |bins| bins.binary_search(bin_name).is_ok())
            })
            .collect()
    }

    pub fn print(&self, opts: &Options) {
        let fetcher = &self.fetcher;
        let bin_files = &self.bin_files;
        let name = &self.name;
        let new_version = &self.new_version;
        let target = fetcher.target();

        debug!(
            "Found a binary install source: {} ({target})",
            fetcher.source_name(),
        );

        warn!(
            "The package {name} v{new_version} ({target}) has been downloaded from {}{}",
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
        let crate_name = self.name.clone();
        self.install_inner(opts)
            .await
            .map_err(|err| err.crate_context(crate_name))
    }

    async fn install_inner(self, opts: Arc<Options>) -> Result<(), BinstallError> {
        let target = if let Some(targets) = opts.desired_targets.get_initialized() {
            Some(targets.first().ok_or(BinstallError::NoViableTargets)?)
        } else {
            None
        };

        if opts.has_overriden_install_path {
            return Err(BinstallError::CargoInstallDoesNotSupportInstallPath);
        }

        let name = &self.name;
        let version = &self.version;

        let cargo = env::var_os("CARGO")
            .map(Cow::Owned)
            .unwrap_or_else(|| Cow::Borrowed(OsStr::new("cargo")));

        let mut cmd = Command::new(cargo);

        cmd.arg("install")
            .arg(name)
            .arg("--version")
            .arg(version)
            .kill_on_drop(true);

        if let Some(target) = target {
            cmd.arg("--target").arg(target);
        }

        if opts.quiet {
            cmd.arg("--quiet");
        }

        if opts.force {
            cmd.arg("--force");
        }

        if opts.locked {
            cmd.arg("--locked");
        }

        if let Some(cargo_root) = &opts.cargo_root {
            cmd.arg("--root").arg(cargo_root);
        }

        if opts.no_track {
            cmd.arg("--no-track");
        }

        if let Some(bins) = &opts.bins {
            for bin in bins {
                cmd.arg("--bin").arg(bin);
            }
        }

        debug!("Running `{}`", format_cmd(&cmd));

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
