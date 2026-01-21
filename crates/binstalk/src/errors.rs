use std::{
    fmt, io, ops,
    path::PathBuf,
    process::{ExitCode, ExitStatus, Termination},
};

use binstalk_downloader::{download::DownloadError, remote::Error as RemoteError};
use binstalk_fetchers::FetchError;
use binstalk_types::cargo_toml_binstall::TargetTripleParseError;
use compact_str::CompactString;
use itertools::Itertools;
use miette::{Diagnostic, Report};
use thiserror::Error;
use tokio::task;
use tracing::{error, warn};

use crate::{
    bins,
    helpers::{
        cargo_toml::Error as CargoTomlError,
        cargo_toml_workspace::Error as LoadManifestFromWSError, gh_api_client::GhApiError,
    },
    registry::{InvalidRegistryError, RegistryError},
};

#[derive(Debug, Error)]
#[error("version string '{v}' is not semver: {err}")]
pub struct VersionParseError {
    pub v: CompactString,
    #[source]
    pub err: semver::Error,
}

#[derive(Debug, Diagnostic, Error)]
#[error("For crate {crate_name}: {err}")]
pub struct CrateContextError {
    crate_name: CompactString,
    #[source]
    #[diagnostic(transparent)]
    err: BinstallError,
}

#[derive(Debug)]
pub struct CrateErrors(Box<[Box<CrateContextError>]>);

impl CrateErrors {
    fn iter(&self) -> impl Iterator<Item = &CrateContextError> + Clone {
        self.0.iter().map(ops::Deref::deref)
    }

    fn get_iter_for<'a, T: 'a>(
        &'a self,
        f: fn(&'a CrateContextError) -> Option<T>,
    ) -> Option<impl Iterator<Item = T> + 'a> {
        let iter = self.iter().filter_map(f);

        if iter.clone().next().is_none() {
            None
        } else {
            Some(iter)
        }
    }
}

impl fmt::Display for CrateErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0.iter().format(", "), f)
    }
}

impl std::error::Error for CrateErrors {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.first().map(|e| e as _)
    }
}

impl miette::Diagnostic for CrateErrors {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new("binstall::many_failure"))
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.iter().filter_map(miette::Diagnostic::severity).max()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(
            self.get_iter_for(miette::Diagnostic::help)?.format("\n"),
        ))
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(
            self.get_iter_for(miette::Diagnostic::url)?.format("\n"),
        ))
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.iter().find_map(miette::Diagnostic::source_code)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let get_iter = || self.iter().filter_map(miette::Diagnostic::labels).flatten();

        if get_iter().next().is_none() {
            None
        } else {
            Some(Box::new(get_iter()))
        }
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        Some(Box::new(
            self.iter().map(|e| e as _).chain(
                self.iter()
                    .filter_map(miette::Diagnostic::related)
                    .flatten(),
            ),
        ))
    }

    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.0.first().map(|err| &**err as _)
    }
}

#[derive(Debug, Error)]
#[error("Invalid pkg-url {pkg_url} for {crate_name}@{version} on {target}: {reason}")]
pub struct InvalidPkgFmtError {
    pub crate_name: CompactString,
    pub version: CompactString,
    pub target: String,
    pub pkg_url: String,
    pub reason: &'static str,
}

/// Error kinds emitted by cargo-binstall.
#[derive(Error, Diagnostic, Debug)]
#[non_exhaustive]
pub enum BinstallError {
    /// Internal: a task could not be joined.
    ///
    /// - Code: `binstall::internal::task_join`
    /// - Exit: 17
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::internal::task_join))]
    TaskJoinError(#[from] task::JoinError),

    /// The installation was cancelled by a user at a confirmation prompt,
    /// or user send a ctrl_c on all platforms or
    /// `SIGINT`, `SIGHUP`, `SIGTERM` or `SIGQUIT` on unix to the program.
    ///
    /// - Code: `binstall::user_abort`
    /// - Exit: 32
    #[error("installation cancelled by user")]
    #[diagnostic(severity(info), code(binstall::user_abort))]
    UserAbort,

    /// Package is not signed and policy requires it.
    ///
    /// - Code: `binstall::signature::invalid`
    /// - Exit: 40
    #[error("Crate {crate_name} is signed and package {package_name} failed verification")]
    #[diagnostic(severity(error), code(binstall::signature::invalid))]
    InvalidSignature {
        crate_name: CompactString,
        package_name: CompactString,
    },

    /// Package is not signed and policy requires it.
    ///
    /// - Code: `binstall::signature::missing`
    /// - Exit: 41
    #[error("Crate {0} does not have signing information")]
    #[diagnostic(severity(error), code(binstall::signature::missing))]
    MissingSignature(CompactString),

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    ///
    /// - Code: `binstall::url_parse`
    /// - Exit: 65
    #[error("Failed to parse url: {0}")]
    #[diagnostic(severity(error), code(binstall::url_parse))]
    UrlParse(#[from] url::ParseError),

    /// Failed to parse template.
    ///
    /// - Code: `binstall::template`
    /// - Exit: 67
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::template))]
    #[source_code(transparent)]
    #[label(transparent)]
    TemplateParseError(
        #[from]
        #[diagnostic_source]
        leon::ParseError,
    ),

    /// Failed to fetch pre-built binaries.
    ///
    /// - Code: `binstall::fetch`
    /// - Exit: 68
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::fetch))]
    #[source_code(transparent)]
    #[label(transparent)]
    FetchError(Box<FetchError>),

    /// Failed to download or failed to decode the body.
    ///
    /// - Code: `binstall::download`
    /// - Exit: 68
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::download))]
    Download(#[from] DownloadError),

    /// A subprocess failed.
    ///
    /// This is often about cargo-install calls.
    ///
    /// - Code: `binstall::subprocess`
    /// - Exit: 70
    #[error("subprocess {command} errored with {status}")]
    #[diagnostic(severity(error), code(binstall::subprocess))]
    SubProcess {
        command: Box<str>,
        status: ExitStatus,
    },

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error("I/O Error: {0}")]
    #[diagnostic(severity(error), code(binstall::io))]
    Io(io::Error),

    /// Unknown registry name
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 75
    #[error("Unknown registry name {0}, env `CARGO_REGISTRIES_{0}_INDEX` nor is it in .cargo/config.toml")]
    #[diagnostic(severity(error), code(binstall::cargo_registry))]
    UnknownRegistryName(CompactString),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 76
    #[error(transparent)]
    #[diagnostic(transparent)]
    RegistryError(#[from] Box<RegistryError>),

    /// The override path to the cargo manifest is invalid or cannot be resolved.
    ///
    /// - Code: `binstall::cargo_manifest_path`
    /// - Exit: 77
    #[error("the --manifest-path is invalid or cannot be resolved")]
    #[diagnostic(severity(error), code(binstall::cargo_manifest_path))]
    CargoManifestPath,

    /// A parsing or validation error in a cargo manifest.
    ///
    /// This should be rare, as manifests are generally fetched from crates.io, which does its own
    /// validation upstream. The most common failure will therefore be for direct repository access
    /// and with the `--manifest-path` option.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 78
    #[error("Failed to parse cargo manifest: {0}")]
    #[diagnostic(
        severity(error),
        code(binstall::cargo_manifest),
        help("If you used --manifest-path, check the Cargo.toml syntax.")
    )]
    CargoManifest(Box<CargoTomlError>),

    /// Failure to parse registry index url
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 79
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::cargo_registry))]
    RegistryParseError(#[from] Box<InvalidRegistryError>),

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
    ///
    /// - Code: `binstall::version::parse`
    /// - Exit: 80
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::version::parse))]
    VersionParse(#[from] Box<VersionParseError>),

    /// The crate@version syntax was used at the same time as the --version option.
    ///
    /// You can't do that as it's ambiguous which should apply.
    ///
    /// - Code: `binstall::conflict::version`
    /// - Exit: 84
    #[error("superfluous version specification")]
    #[diagnostic(
        severity(error),
        code(binstall::conflict::version),
        help("You cannot use both crate@version and the --version option. Remove one.")
    )]
    SuperfluousVersionOption,

    /// No binaries were found for the crate.
    ///
    /// When installing, either the binaries are specified in the crate's Cargo.toml, or they're
    /// inferred from the crate layout (e.g. src/main.rs or src/bins/name.rs). If no binaries are
    /// found through these methods, we can't know what to install!
    ///
    /// - Code: `binstall::resolve::binaries`
    /// - Exit: 86
    #[error("no binaries specified nor inferred")]
    #[diagnostic(
        severity(error),
        code(binstall::resolve::binaries),
        help("This crate doesn't specify any binaries, so there's nothing to install.")
    )]
    UnspecifiedBinaries,

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
    #[diagnostic(
        severity(error),
        code(binstall::targets::none_host),
        help("Try to specify --target")
    )]
    NoViableTargets,

    /// Failed to find or install binaries.
    ///
    /// - Code: `binstall::bins`
    /// - Exit: 88
    #[error("failed to find or install binaries: {0}")]
    #[diagnostic(
        severity(error),
        code(binstall::targets::none_host),
        help("Try to specify --target")
    )]
    BinFile(#[from] bins::Error),

    /// `Cargo.toml` of the crate does not have section "Package".
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 89
    #[error("Cargo.toml of crate {0} does not have section \"Package\"")]
    #[diagnostic(severity(error), code(binstall::cargo_manifest))]
    CargoTomlMissingPackage(CompactString),

    /// bin-dir configuration provided generates duplicate source path.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 90
    #[error("bin-dir configuration provided generates duplicate source path: {path}")]
    #[diagnostic(severity(error), code(binstall::SourceFilePath))]
    DuplicateSourceFilePath { path: PathBuf },

    /// Fallback to `cargo-install` is disabled.
    ///
    /// - Code: `binstall::no_fallback_to_cargo_install`
    /// - Exit: 94
    #[error("Fallback to cargo-install is disabled")]
    #[diagnostic(severity(error), code(binstall::no_fallback_to_cargo_install))]
    NoFallbackToCargoInstall,

    /// Fallback to `cargo-install` is disabled.
    ///
    /// - Code: `binstall::invalid_pkg_fmt`
    /// - Exit: 95
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::invalid_pkg_fmt))]
    InvalidPkgFmt(Box<InvalidPkgFmtError>),

    /// Request to GitHub API failed
    ///
    /// - Code: `binstall::gh_api_failure`
    /// - Exit: 96
    #[error("Request to GitHub API failed: {0}")]
    #[diagnostic(severity(error), code(binstall::gh_api_failure))]
    GhApiErr(#[source] Box<GhApiError>),

    /// Failed to parse target triple
    ///
    /// - Code: `binstall::target_triple_parse_error`
    /// - Exit: 97
    #[error("Failed to parse target triple: {0}")]
    #[diagnostic(severity(error), code(binstall::target_triple_parse_error))]
    TargetTripleParseError(#[source] Box<TargetTripleParseError>),

    /// Failed to shallow clone git repository
    ///
    /// - Code: `binstall::git`
    /// - Exit: 98
    #[cfg(feature = "git")]
    #[error("Failed to shallow clone git repository: {0}")]
    #[diagnostic(severity(error), code(binstall::git))]
    GitError(#[from] crate::helpers::git::GitError),

    /// Failed to load manifest from workspace
    ///
    /// - Code: `binstall::load_manifest_from_workspace`
    /// - Exit: 99
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::load_manifest_from_workspace))]
    LoadManifestFromWSError(#[from] Box<LoadManifestFromWSError>),

    /// `cargo-install` does not support `--install-path`
    ///
    /// - Code: `binstall::cargo_install_does_not_support_install_path`
    /// - Exit: 100
    #[error("cargo-install does not support `--install-path`")]
    #[diagnostic(
        severity(error),
        code(binatall::cargo_install_does_not_support_install_path)
    )]
    CargoInstallDoesNotSupportInstallPath,

    /// A wrapped error providing the context of which crate the error is about.
    #[error(transparent)]
    #[diagnostic(transparent)]
    CrateContext(Box<CrateContextError>),

    /// A wrapped error for failures of multiple crates when `--continue-on-failure` is specified.
    #[error(transparent)]
    #[diagnostic(transparent)]
    Errors(CrateErrors),
}

impl BinstallError {
    fn exit_number(&self) -> u8 {
        use BinstallError::*;
        let code: u8 = match self {
            TaskJoinError(_) => 17,
            UserAbort => 32,
            InvalidSignature { .. } => 40,
            MissingSignature(_) => 41,
            UrlParse(_) => 65,
            TemplateParseError(..) => 67,
            FetchError(..) => 68,
            Download(_) => 68,
            SubProcess { .. } => 70,
            Io(_) => 74,
            UnknownRegistryName(_) => 75,
            RegistryError { .. } => 76,
            CargoManifestPath => 77,
            CargoManifest { .. } => 78,
            RegistryParseError(..) => 79,
            VersionParse { .. } => 80,
            SuperfluousVersionOption => 84,
            UnspecifiedBinaries => 86,
            NoViableTargets => 87,
            BinFile(_) => 88,
            CargoTomlMissingPackage(_) => 89,
            DuplicateSourceFilePath { .. } => 90,
            NoFallbackToCargoInstall => 94,
            InvalidPkgFmt(..) => 95,
            GhApiErr(..) => 96,
            TargetTripleParseError(..) => 97,
            #[cfg(feature = "git")]
            GitError(_) => 98,
            LoadManifestFromWSError(_) => 99,
            CargoInstallDoesNotSupportInstallPath => 100,
            CrateContext(context) => context.err.exit_number(),
            Errors(errors) => (errors.0)[0].err.exit_number(),
        };

        // reserved codes
        debug_assert!(code != 64 && code != 16 && code != 1 && code != 2 && code != 0);

        code
    }

    /// The recommended exit code for this error.
    ///
    /// This will never output:
    /// - 0 (success)
    /// - 1 and 2 (catchall and shell)
    /// - 16 (binstall errors not handled here)
    /// - 64 (generic error)
    pub fn exit_code(&self) -> ExitCode {
        self.exit_number().into()
    }

    /// Add crate context to the error
    pub fn crate_context(self, crate_name: impl Into<CompactString>) -> Self {
        self.crate_context_inner(crate_name.into())
    }

    fn crate_context_inner(self, crate_name: CompactString) -> Self {
        match self {
            Self::CrateContext(mut crate_context_error) => {
                crate_context_error.crate_name = crate_name;
                Self::CrateContext(crate_context_error)
            }
            err => Self::CrateContext(Box::new(CrateContextError { err, crate_name })),
        }
    }

    pub fn crate_errors(mut errors: Vec<Box<CrateContextError>>) -> Option<Self> {
        if errors.is_empty() {
            None
        } else if errors.len() == 1 {
            Some(Self::CrateContext(errors.pop().unwrap()))
        } else {
            Some(Self::Errors(CrateErrors(errors.into_boxed_slice())))
        }
    }
}

impl Termination for BinstallError {
    fn report(self) -> ExitCode {
        let code = self.exit_code();
        if let BinstallError::UserAbort = self {
            warn!("Installation cancelled");
        } else {
            error!("Fatal error:\n{:?}", Report::new(self));
        }

        code
    }
}

impl From<io::Error> for BinstallError {
    fn from(err: io::Error) -> Self {
        err.downcast::<BinstallError>()
            .unwrap_or_else(BinstallError::Io)
    }
}

impl From<BinstallError> for io::Error {
    fn from(e: BinstallError) -> io::Error {
        match e {
            BinstallError::Io(io_error) => io_error,
            e => io::Error::other(e),
        }
    }
}

impl From<RemoteError> for BinstallError {
    fn from(e: RemoteError) -> Self {
        DownloadError::from(e).into()
    }
}

impl From<CargoTomlError> for BinstallError {
    fn from(e: CargoTomlError) -> Self {
        BinstallError::CargoManifest(Box::new(e))
    }
}

impl From<InvalidPkgFmtError> for BinstallError {
    fn from(e: InvalidPkgFmtError) -> Self {
        BinstallError::InvalidPkgFmt(Box::new(e))
    }
}

impl From<GhApiError> for BinstallError {
    fn from(e: GhApiError) -> Self {
        BinstallError::GhApiErr(Box::new(e))
    }
}

impl From<TargetTripleParseError> for BinstallError {
    fn from(e: TargetTripleParseError) -> Self {
        BinstallError::TargetTripleParseError(Box::new(e))
    }
}

impl From<RegistryError> for BinstallError {
    fn from(e: RegistryError) -> Self {
        BinstallError::RegistryError(Box::new(e))
    }
}

impl From<InvalidRegistryError> for BinstallError {
    fn from(e: InvalidRegistryError) -> Self {
        BinstallError::RegistryParseError(Box::new(e))
    }
}

impl From<LoadManifestFromWSError> for BinstallError {
    fn from(e: LoadManifestFromWSError) -> Self {
        BinstallError::LoadManifestFromWSError(Box::new(e))
    }
}

impl From<FetchError> for BinstallError {
    fn from(e: FetchError) -> Self {
        BinstallError::FetchError(Box::new(e))
    }
}
