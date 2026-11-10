//! Error handling of cargo-binstall.
//!
//! Errors are type-erased trees of [`Error`]s (see [`binstalk_types::errors`]).
//! Most of them are only reported to the user, so they carry no type
//! information besides their message.
//!
//! An error that ends the program is tagged with an [`Exit`], which decides
//! the exit code of cargo-binstall and the diagnostic code shown to the user.

use std::process;

use compact_str::CompactString;
use strum::EnumProperty as _;
use strum_macros::EnumProperty;
use thiserror::Error as ThisError;

pub use binstalk_types::{errors::*, message};

/// Exit codes of cargo-binstall.
///
/// The first [`Exit`] found in the error tree (depth-first) decides the exit
/// code, see [`exit_code`].
///
/// This will never be:
/// - 0 (success)
/// - 1 and 2 (catchall and shell)
/// - 16 (errors without an [`Exit`])
/// - 64 (generic error)
#[derive(Clone, Copy, Debug, PartialEq, Eq, ThisError, EnumProperty)]
#[repr(u8)]
#[non_exhaustive]
pub enum Exit {
    /// Internal: a task could not be joined.
    ///
    /// - Code: `binstall::internal::task_join`
    /// - Exit: 17
    #[error("failed to join task")]
    #[strum(props(code = "binstall::internal::task_join"))]
    TaskJoin = 17,

    /// The installation was cancelled by a user at a confirmation prompt,
    /// or user send a ctrl_c on all platforms or
    /// `SIGINT`, `SIGHUP`, `SIGTERM` or `SIGQUIT` on unix to the program.
    ///
    /// - Code: `binstall::user_abort`
    /// - Exit: 32
    #[error("installation cancelled by user")]
    #[strum(props(code = "binstall::user_abort"))]
    UserAbort = 32,

    /// Package is signed but failed verification.
    ///
    /// - Code: `binstall::signature::invalid`
    /// - Exit: 40
    #[error("package failed signature verification")]
    #[strum(props(code = "binstall::signature::invalid"))]
    InvalidSignature = 40,

    /// Package is not signed and policy requires it.
    ///
    /// - Code: `binstall::signature::missing`
    /// - Exit: 41
    #[error("package does not have signing information")]
    #[strum(props(code = "binstall::signature::missing"))]
    MissingSignature = 41,

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    ///
    /// - Code: `binstall::url_parse`
    /// - Exit: 65
    #[error("Failed to parse url")]
    #[strum(props(code = "binstall::url_parse"))]
    UrlParse = 65,

    /// Failed to parse template.
    ///
    /// - Code: `binstall::template`
    /// - Exit: 67
    #[error("Failed to parse template")]
    #[strum(props(code = "binstall::template"))]
    TemplateParse = 67,

    /// Failed to fetch pre-built binaries, or failed to download or decode
    /// the body.
    ///
    /// - Code: `binstall::fetch`
    /// - Exit: 68
    #[error("Failed to fetch pre-built binaries")]
    #[strum(props(code = "binstall::fetch"))]
    Fetch = 68,

    /// A subprocess failed.
    ///
    /// This is often about cargo-install calls.
    ///
    /// - Code: `binstall::subprocess`
    /// - Exit: 70
    #[error("subprocess failed")]
    #[strum(props(code = "binstall::subprocess"))]
    SubProcess = 70,

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error("I/O Error")]
    #[strum(props(code = "binstall::io"))]
    Io = 74,

    /// Unknown registry name
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 75
    #[error("Unknown registry name")]
    #[strum(props(code = "binstall::cargo_registry"))]
    UnknownRegistryName = 75,

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 76
    #[error("Failed to fetch crate from registry")]
    #[strum(props(code = "binstall::cargo_registry"))]
    Registry = 76,

    /// The override path to the cargo manifest is invalid or cannot be resolved.
    ///
    /// - Code: `binstall::cargo_manifest_path`
    /// - Exit: 77
    #[error("the --manifest-path is invalid or cannot be resolved")]
    #[strum(props(code = "binstall::cargo_manifest_path"))]
    CargoManifestPath = 77,

    /// A parsing or validation error in a cargo manifest.
    ///
    /// This should be rare, as manifests are generally fetched from crates.io, which does its own
    /// validation upstream. The most common failure will therefore be for direct repository access
    /// and with the `--manifest-path` option.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 78
    #[error("Failed to parse cargo manifest")]
    #[strum(props(
        code = "binstall::cargo_manifest",
        help = "If you used --manifest-path, check the Cargo.toml syntax."
    ))]
    CargoManifest = 78,

    /// Failure to parse registry index url
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 79
    #[error("Failed to parse registry index url")]
    #[strum(props(code = "binstall::cargo_registry"))]
    RegistryParse = 79,

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
    ///
    /// - Code: `binstall::version::parse`
    /// - Exit: 80
    #[error("version string is not semver")]
    #[strum(props(code = "binstall::version::parse"))]
    VersionParse = 80,

    /// The crate@version syntax was used at the same time as the --version option.
    ///
    /// You can't do that as it's ambiguous which should apply.
    ///
    /// - Code: `binstall::conflict::version`
    /// - Exit: 84
    #[error("superfluous version specification")]
    #[strum(props(
        code = "binstall::conflict::version",
        help = "You cannot use both crate@version and the --version option. Remove one."
    ))]
    SuperfluousVersionOption = 84,

    /// No binaries were found for the crate.
    ///
    /// When installing, either the binaries are specified in the crate's Cargo.toml, or they're
    /// inferred from the crate layout (e.g. src/main.rs or src/bins/name.rs). If no binaries are
    /// found through these methods, we can't know what to install!
    ///
    /// - Code: `binstall::resolve::binaries`
    /// - Exit: 86
    #[error("no binaries specified nor inferred")]
    #[strum(props(
        code = "binstall::resolve::binaries",
        help = "This crate doesn't specify any binaries, so there's nothing to install."
    ))]
    UnspecifiedBinaries = 86,

    /// No viable targets were found.
    ///
    /// When installing, we attempt to find which targets the host (your computer) supports, and
    /// discover builds for these targets from the remote binary source. This error occurs when we
    /// fail to discover the host's target.
    ///
    /// You should in this case specify --target manually.
    ///
    /// - Code: `binstall::targets::none_host`
    /// - Exit: 87
    #[error("failed to discovered a viable target from the host")]
    #[strum(props(
        code = "binstall::targets::none_host",
        help = "Try to specify --target"
    ))]
    NoViableTargets = 87,

    /// Failed to find or install binaries.
    ///
    /// - Code: `binstall::bins`
    /// - Exit: 88
    #[error("failed to find or install binaries")]
    #[strum(props(code = "binstall::bins"))]
    BinFile = 88,

    /// `Cargo.toml` of the crate does not have section "Package".
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 89
    #[error("Cargo.toml does not have section \"Package\"")]
    #[strum(props(code = "binstall::cargo_manifest"))]
    CargoTomlMissingPackage = 89,

    /// bin-dir configuration provided generates duplicate source path.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 90
    #[error("bin-dir configuration provided generates duplicate source path")]
    #[strum(props(code = "binstall::cargo_manifest"))]
    DuplicateSourceFilePath = 90,

    /// Fallback to `cargo-install` is disabled.
    ///
    /// - Code: `binstall::no_fallback_to_cargo_install`
    /// - Exit: 94
    #[error("Fallback to cargo-install is disabled")]
    #[strum(props(code = "binstall::no_fallback_to_cargo_install"))]
    NoFallbackToCargoInstall = 94,

    /// The `pkg-url` or `pkg-fmt` is invalid.
    ///
    /// - Code: `binstall::invalid_pkg_fmt`
    /// - Exit: 95
    #[error("invalid pkg-url or pkg-fmt")]
    #[strum(props(code = "binstall::invalid_pkg_fmt"))]
    InvalidPkgFmt = 95,

    /// Request to GitHub API failed
    ///
    /// - Code: `binstall::gh_api_failure`
    /// - Exit: 96
    #[error("Request to GitHub API failed")]
    #[strum(props(code = "binstall::gh_api_failure"))]
    GhApi = 96,

    /// Failed to parse target triple
    ///
    /// - Code: `binstall::target_triple_parse_error`
    /// - Exit: 97
    #[error("Failed to parse target triple")]
    #[strum(props(code = "binstall::target_triple_parse_error"))]
    TargetTripleParse = 97,

    /// Failed to shallow clone git repository
    ///
    /// - Code: `binstall::git`
    /// - Exit: 98
    #[error("Failed to shallow clone git repository")]
    #[strum(props(code = "binstall::git"))]
    Git = 98,

    /// Failed to load manifest from workspace
    ///
    /// - Code: `binstall::load_manifest_from_workspace`
    /// - Exit: 99
    #[error("Failed to load manifest from workspace")]
    #[strum(props(code = "binstall::load_manifest_from_workspace"))]
    LoadManifestFromWorkspace = 99,

    /// `cargo-install` does not support `--install-path`
    ///
    /// - Code: `binstall::cargo_install_does_not_support_install_path`
    /// - Exit: 100
    #[error("cargo-install does not support `--install-path`")]
    #[strum(props(code = "binstall::cargo_install_does_not_support_install_path"))]
    CargoInstallDoesNotSupportInstallPath = 100,
}

impl Exit {
    /// The exit code.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The diagnostic code shown to the user.
    pub fn diagnostic_code(self) -> &'static str {
        self.get_str("code")
            .expect("every variant has a diagnostic code")
    }

    /// A hint on how to fix the error.
    pub fn help(self) -> Option<&'static str> {
        self.get_str("help")
    }

    /// Create an error tagged with `self`, with `message` describing it.
    #[track_caller]
    pub fn with(self, message: Message) -> Error {
        message.raise().raise(self).into()
    }
}

impl From<Exit> for process::ExitCode {
    fn from(exit: Exit) -> Self {
        exit.code().into()
    }
}

/// Find the first frame in `frame`'s tree (depth-first, pre-order) whose error
/// is a `T` and for which `f` returns `Some`.
fn find_map<'a, T: std::error::Error + 'static, U>(
    frame: &'a Frame,
    f: &mut impl FnMut(&'a T) -> Option<U>,
) -> Option<U> {
    frame
        .error()
        .downcast_ref::<T>()
        .and_then(&mut *f)
        .or_else(|| frame.children().iter().find_map(|child| find_map(child, f)))
}

/// The exit code of `err`: the first [`Exit`] in the tree, i.e. the first
/// crate that failed when multiple crates failed, or 16 if there is none.
pub fn exit_code(err: &Error) -> process::ExitCode {
    find(err.frame())
        .map(|exit: &Exit| exit.code())
        .unwrap_or(16)
        .into()
}

/// Returns true if the installation was cancelled by the user, i.e. there is
/// an [`Exit::UserAbort`] anywhere in `frame`.
pub fn is_user_abort(frame: &Frame) -> bool {
    find_map(frame, &mut |exit: &Exit| {
        (*exit == Exit::UserAbort).then_some(())
    })
    .is_some()
}

/// Context of which crate the child error is about.
#[derive(Debug, ThisError)]
#[error("For crate {0}")]
pub struct CrateContext(CompactString);

/// Add crate context to `err`, unless it already has one.
#[track_caller]
pub fn crate_context(err: Error, crate_name: impl Into<CompactString>) -> Error {
    if err.downcast_ref::<CrateContext>().is_some() {
        err
    } else {
        err.raise(CrateContext(crate_name.into())).into()
    }
}

/// Combine errors of multiple crates, return `None` if `errors` is empty.
#[track_caller]
pub fn crate_errors(mut errors: Vec<Error>) -> Option<Error> {
    match errors.len() {
        0 => None,
        1 => errors.pop(),
        n => Some(
            errors
                .into_iter()
                .raise(message!("Failed to install {n} crates"))
                .into(),
        ),
    }
}
