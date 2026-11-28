use std::{
    borrow::Cow,
    env,
    ffi::OsStr,
    fmt, fs, iter,
    mem::DropGuard,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use binstalk_bins::BinFile;
use binstalk_types::redact_url_userinfo;
use command_group::{AsyncCommandGroup, AsyncGroupChild};
use compact_str::{CompactString, ToCompactString};
use either::Either;
use itertools::Itertools;
use semver::Version;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

#[cfg(feature = "git")]
use crate::ops::{CargoTomlFetchOverride, GitReference};
use crate::{
    bins,
    errors::BinstallError,
    fetchers::Found,
    manifests::crate_info::{CrateInfo, CrateSource},
    ops::Options,
};

pub struct ResolutionFetch {
    /// The package found, already downloaded and extracted.
    pub found: Box<dyn Found>,
    pub new_version: Version,
    /// Publish time (RFC 3339) from the registry index, if known
    pub pubtime: Option<CompactString>,
    pub name: CompactString,
    pub version_req: CompactString,
    pub bin_files: Vec<bins::BinFile>,
    pub source: CrateSource,
}

pub struct ResolutionSource {
    pub name: CompactString,
    pub version: CompactString,
    /// Publish time (RFC 3339) from the registry index, if known
    pub pubtime: Option<CompactString>,
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

        // Generate symlinks: `link` is only set without `--no-symlinks`, but
        // the aliases are always created.
        for file in &self.bin_files {
            install_link(file)?;
        }

        Ok(CrateInfo {
            name: self.name,
            version_req: self.version_req,
            current_version: self.new_version,
            source: self.source,
            target: self.found.target().to_compact_string(),
            bins: Self::resolve_bins(&opts.bins, self.bin_files),
            pkg_url_hint: self.found.pkg_url_hint(),
        })
    }

    fn resolve_bins(
        user_specified_bins: &Option<Vec<CompactString>>,
        crate_bin_files: Vec<BinFile>,
    ) -> Vec<CompactString> {
        // We need to filter crate_bin_files by user_specified_bins in case the prebuilt doesn't
        // have featured-gated (optional) binary (gated behind feature).
        //
        // Aliases follow their parent binary: they are recorded (so uninstall
        // removes them) whenever the binary they point at is installed.
        crate_bin_files
            .into_iter()
            .filter(|bin| {
                user_specified_bins
                    .as_ref()
                    .is_none_or(|bins| bins.binary_search(&bin.base_name).is_ok())
            })
            .flat_map(
                |BinFile {
                     base_name, aliases, ..
                 }| {
                    let aliases = aliases
                        .into_iter()
                        .map(|alias| alias.file_name().unwrap().to_string_lossy().into());
                    iter::once(base_name).chain(aliases)
                },
            )
            .collect()
    }

    pub fn print(&self, opts: &Options) {
        let found = &self.found;
        let bin_files = &self.bin_files;
        let name = &self.name;
        let new_version = &self.new_version;
        let target = found.target();
        let released = self
            .pubtime
            .as_deref()
            .map(|pubtime| format!(", released {}", release_date(pubtime)))
            .unwrap_or_default();

        debug!(
            "Found a binary install source: {} ({target})",
            found.source_name(),
        );

        warn!(
            "The package {name} v{new_version} ({target}{released}) has been downloaded from {}{}",
            if found.is_third_party() {
                "third-party source "
            } else {
                ""
            },
            found.source_name()
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

        let Some(path) = env::var_os("PATH") else {
            return;
        };
        for file in bin_files {
            let Some(other) = shadowing_file(&path, &opts.install_path, &file.base_name) else {
                continue;
            };
            warn!(
                "{} is also at {}, which PATH finds before the one installed to {}",
                file.base_name,
                other.display(),
                opts.install_path.display(),
            );
        }
    }
}

/// An executable named `file_name` in a dir of `path` (a `PATH`-like list)
/// that comes before `install_path`, so it runs instead of the one installed
/// to `install_path`.
///
/// If `install_path` is not in `path` at all, any such executable runs instead.
fn shadowing_file(path: &OsStr, install_path: &Path, file_name: &str) -> Option<PathBuf> {
    // A closure can't tie the returned `Cow` to its argument's lifetime.
    fn canonicalize(dir: &Path) -> Cow<'_, Path> {
        fs::canonicalize(dir).map(Cow::Owned).unwrap_or(dir.into())
    }

    let canonical_install_path = canonicalize(install_path);
    // Not set outside of Windows, so the default doesn't allocate there.
    let pathext = env::var("PATHEXT").map_or(Cow::Borrowed(".COM;.EXE;.BAT;.CMD"), Cow::Owned);

    env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        // Compare as is first, to skip `canonicalize` in the common case.
        .take_while(|dir| dir != install_path && canonicalize(dir) != canonical_install_path)
        .find_map(|dir| find_executable(dir, file_name, &pathext))
}

/// The file in `dir` that running `file_name` from `PATH` would pick, if any.
///
/// Each candidate is tried in the `dir` buffer itself, so it needs no other.
fn find_executable(dir: PathBuf, file_name: &str, pathext: &str) -> Option<PathBuf> {
    let mut path = dir;
    path.push(file_name);

    if !cfg!(windows) {
        return is_executable(&path).then_some(path);
    }

    // On Windows only a file with an extension listed in `PATHEXT` runs from
    // `PATH`, and typing `fnm` picks `fnm` with the first of them that exists,
    // so an earlier `fnm.cmd` also shadows `fnm.exe`.
    let exts = || {
        pathext
            .split(';')
            .filter_map(|ext| ext.strip_prefix('.'))
            .filter(|ext| !ext.is_empty())
    };
    let runs = path
        .extension()
        .is_some_and(|ext| exts().any(|runnable| ext.eq_ignore_ascii_case(runnable)));
    if !runs {
        // The installed file can't be run by its name either.
        return None;
    }

    for ext in exts() {
        path.set_extension(ext);
        if is_executable(&path) {
            return Some(path);
        }
    }

    None
}

/// `path` is a file (following symlinks) that can be run: on unix one with an
/// execute bit set, on Windows any file, since it is picked by its extension.
fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    #[cfg(unix)]
    let executable = std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111 != 0;
    #[cfg(not(unix))]
    let executable = true;

    metadata.is_file() && executable
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

        cmd.arg("install").arg(name).kill_on_drop(true);

        // Do not leak tokens to build scripts via environment variables.
        cmd.env_remove("GITHUB_TOKEN").env_remove("GH_TOKEN");

        match &opts.cargo_toml_fetch_override {
            // No `--version`: the reference decides what is built, and the
            // version may differ from the one in the repository, see
            // `PackageInfo::resolve`.
            #[cfg(feature = "git")]
            Some(CargoTomlFetchOverride::Git(_, reference, git_url)) => {
                apply_git_selection(&mut cmd, git_url, reference)
            }
            _ => {
                cmd.arg("--version").arg(version);
                apply_registry_selection(
                    &mut cmd,
                    opts.cargo_install_registry.as_deref(),
                    opts.cargo_install_index.as_deref(),
                    opts.registry.min_publish_age(),
                );
            }
        }

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

        if let Some(features) = &opts.features {
            for feature in features {
                cmd.arg("--features").arg(feature);
            }
        }

        if opts.no_default_features {
            cmd.arg("--no-default-features");
        }

        if opts.all_features {
            cmd.arg("--all-features");
        }

        debug!("Running `{}`", format_cmd(&cmd));

        if !opts.dry_run {
            let child = opts
                .jobserver_client
                .get()
                .await?
                .configure_and_run(&mut cmd, |cmd| cmd.group_spawn())?;

            debug!("Spawned command pid={:?}", child.id());

            let mut child = kill_group_on_drop(child);
            let status = child.wait().await;
            // The group has exited (or waiting failed), do not kill anything
            // left behind, such as `sccache` servers on Windows (which stay in
            // the job object).
            DropGuard::dismiss(child);
            let status = status?;
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
        let released = self
            .pubtime
            .as_deref()
            .map(|pubtime| format!(" (released {})", release_date(pubtime)))
            .unwrap_or_default();

        warn!(
            "The package {} v{}{released} will be installed from source (with cargo)",
            self.name, self.version
        )
    }
}

/// An RFC 3339 timestamp starts with a fixed-width `YYYY-MM-DD` date.
fn release_date(pubtime: &str) -> &str {
    pubtime.get(..10).unwrap_or(pubtime)
}

/// Kills the whole process group (unix) / job object (windows) of
/// `cargo install` if it is dropped, e.g. when the install task is
/// cancelled on Ctrl-C/SIGTERM.
///
/// `cargo` runs in its own process group, so it does not receive the
/// terminal's SIGINT, and tokio's `kill_on_drop` only kills `cargo` itself,
/// leaving `rustc`/build scripts running.
fn kill_group_on_drop(
    child: AsyncGroupChild,
) -> DropGuard<AsyncGroupChild, impl FnOnce(AsyncGroupChild)> {
    DropGuard::new(child, |mut child| {
        if let Err(err) = child.start_kill() {
            warn!("Failed to kill `cargo install` process group: {err}");
        }
    })
}

fn apply_registry_selection(
    cmd: &mut Command,
    registry: Option<&str>,
    index: Option<&str>,
    min_publish_age: Option<Duration>,
) {
    if let Some(registry) = registry {
        cmd.arg("--registry").arg(registry);
    } else if let Some(index) = index {
        cmd.arg("--index").arg(index);
    }

    // Forward `--min-publish-age`, overriding any per-registry config of the
    // selected registry. cargo 1.100 `cargo install` ignores it, this is for
    // when cargo starts honoring it.
    if let Some(min_publish_age) = min_publish_age {
        let key = match (registry, index) {
            (Some(registry), _) => format!("registries.{registry}.min-publish-age"),
            (None, Some(_)) => "registry.global-min-publish-age".to_owned(),
            (None, None) => "registries.crates-io.min-publish-age".to_owned(),
        };
        let value = match min_publish_age.as_secs() {
            0 => "0".to_owned(),
            secs => format!("{secs} seconds"),
        };
        cmd.arg("--config").arg(format!("{key}=\"{value}\""));
    }
}

/// Make `cargo install` build the crate from the repository `--git` points
/// to, instead of the crate with the same name in the registry.
///
/// `git_url` is the url as it was given, with its password if it has one.
/// `format_cmd` hides it when the command is printed.
#[cfg(feature = "git")]
fn apply_git_selection(cmd: &mut Command, git_url: &str, reference: &GitReference) {
    cmd.arg("--git").arg(git_url);

    let (flag, value) = match reference {
        GitReference::DefaultBranch => return,
        GitReference::Branch(branch) => ("--branch", branch),
        GitReference::Tag(tag) => ("--tag", tag),
        GitReference::Rev(rev) => ("--rev", rev),
    };
    cmd.arg(flag).arg(value);
}

fn format_cmd(cmd: &Command) -> impl fmt::Display + '_ {
    let cmd = cmd.as_std();

    let program = Either::Left(Path::new(cmd.get_program()).display());

    // An index passed to cargo may hold a token.
    let program_args = cmd
        .get_args()
        .map(|arg| redact_url_userinfo(arg.to_string_lossy()))
        .map(Either::Right);

    iter::once(program).chain(program_args).format(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_registry_selection_uses_registry_name() {
        let mut cmd = Command::new("cargo");

        apply_registry_selection(&mut cmd, Some("private-test"), None, None);

        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["--registry", "private-test"]);
    }

    #[test]
    fn test_format_cmd_redacts_index_credentials() {
        let mut cmd = Command::new("cargo");
        cmd.arg("install").arg("a");

        apply_registry_selection(
            &mut cmd,
            None,
            Some("sparse+https://user:secret-token@registry.example.com/index/"),
            None,
        );

        assert_eq!(
            format_cmd(&cmd).to_string(),
            "cargo install a --index sparse+https://<redacted>@registry.example.com/index/"
        );
    }

    #[test]
    fn test_apply_registry_selection_uses_index_when_registry_name_missing() {
        let mut cmd = Command::new("cargo");

        apply_registry_selection(
            &mut cmd,
            None,
            Some("sparse+https://registry.example.com/index/"),
            None,
        );

        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            ["--index", "sparse+https://registry.example.com/index/"]
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_apply_git_selection() {
        let url = "https://github.com/cargo-bins/cargo-binstall";

        for (reference, expected) in [
            (GitReference::DefaultBranch, &[][..]),
            (GitReference::Branch("main".into()), &["--branch", "main"]),
            (GitReference::Tag("v1.0.0".into()), &["--tag", "v1.0.0"]),
            (GitReference::Rev("abc123".into()), &["--rev", "abc123"]),
        ] {
            let mut cmd = Command::new("cargo");
            apply_git_selection(&mut cmd, url, &reference);

            let args: Vec<_> = cmd
                .as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(args, [&["--git", url], expected].concat(), "{reference:?}");
        }
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_apply_git_selection_keeps_password_but_hides_it_in_logs() {
        let mut cmd = Command::new("cargo");
        cmd.arg("install").arg("a");
        apply_git_selection(
            &mut cmd,
            "https://user:secret-token@example.com/o/r",
            &GitReference::DefaultBranch,
        );

        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert_eq!(
            args,
            [
                "install",
                "a",
                "--git",
                "https://user:secret-token@example.com/o/r"
            ]
        );
        assert_eq!(
            format_cmd(&cmd).to_string(),
            "cargo install a --git https://<redacted>@example.com/o/r"
        );
    }

    #[test]
    fn test_apply_registry_selection_forwards_min_publish_age() {
        let args = |registry, index, min_publish_age| {
            let mut cmd = Command::new("cargo");
            apply_registry_selection(&mut cmd, registry, index, min_publish_age);
            cmd.as_std()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let week = Some(Duration::from_secs(7 * 24 * 60 * 60));

        assert_eq!(
            args(Some("private-test"), None, week),
            [
                "--registry",
                "private-test",
                "--config",
                "registries.private-test.min-publish-age=\"604800 seconds\"",
            ]
        );
        assert_eq!(
            args(
                None,
                Some("sparse+https://registry.example.com/index/"),
                week
            ),
            [
                "--index",
                "sparse+https://registry.example.com/index/",
                "--config",
                "registry.global-min-publish-age=\"604800 seconds\"",
            ]
        );
        assert_eq!(
            args(None, None, Some(Duration::ZERO)),
            ["--config", "registries.crates-io.min-publish-age=\"0\""]
        );
        assert!(args(None, None, None).is_empty());
    }
}

#[cfg(test)]
mod shadowing_tests {
    use super::*;

    #[test]
    fn shadowing_file_is_found_before_the_install_path() {
        let tmp = tempfile::tempdir().unwrap();
        let [early, install, late] = ["early", "install", "late"].map(|dir| {
            let dir = tmp.path().join(dir);
            fs::create_dir(&dir).unwrap();
            dir
        });
        let write_exe = |path: PathBuf| {
            fs::write(&path, b"").unwrap();
            #[cfg(unix)]
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        };
        write_exe(install.join("fnm"));
        write_exe(late.join("fnm"));

        let path = |dirs: &[&Path]| env::join_paths(dirs).unwrap();

        // Nothing earlier in PATH has it, and later ones don't matter.
        let p = path(&[&early, &install, &late]);
        assert_eq!(shadowing_file(&p, &install, "fnm"), None);

        // An earlier dir has it.
        write_exe(early.join("fnm"));
        assert_eq!(shadowing_file(&p, &install, "fnm"), Some(early.join("fnm")));

        // The install path comes first.
        let p = path(&[&install, &early]);
        assert_eq!(shadowing_file(&p, &install, "fnm"), None);

        // The install path is not in PATH, so any match runs instead.
        let p = path(&[&late]);
        assert_eq!(shadowing_file(&p, &install, "fnm"), Some(late.join("fnm")));

        // A dir with that name is not a file that runs.
        fs::create_dir(early.join("dir")).unwrap();
        let p = path(&[&early, &install]);
        assert_eq!(shadowing_file(&p, &install, "dir"), None);
    }

    /// A file without an execute bit can't run, so it doesn't shadow anything.
    #[cfg(unix)]
    #[test]
    fn non_executable_file_does_not_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let [early, install] = ["early", "install"].map(|dir| {
            let dir = tmp.path().join(dir);
            fs::create_dir(&dir).unwrap();
            dir
        });
        fs::write(early.join("fnm"), b"").unwrap();
        fs::set_permissions(
            early.join("fnm"),
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();

        let p = env::join_paths([&early, &install]).unwrap();
        assert_eq!(shadowing_file(&p, &install, "fnm"), None);
    }

    #[cfg(windows)]
    #[test]
    fn other_pathext_extensions_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let [early, install] = ["early", "install"].map(|dir| {
            let dir = tmp.path().join(dir);
            fs::create_dir(&dir).unwrap();
            dir
        });
        fs::write(early.join("fnm.cmd"), b"").unwrap();

        let p = env::join_paths([&early, &install]).unwrap();
        let found = shadowing_file(&p, &install, "fnm.exe").unwrap();
        assert!(
            found.file_name().unwrap().eq_ignore_ascii_case("fnm.cmd"),
            "{found:?}"
        );

        // A file without an extension listed in `PATHEXT` doesn't run.
        fs::write(early.join("fnm"), b"").unwrap();
        assert_eq!(shadowing_file(&p, &install, "fnm"), None);
    }
}
