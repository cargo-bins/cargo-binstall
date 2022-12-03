use std::{
    io,
    path::PathBuf,
    process::{ExitCode, ExitStatus, Termination},
};

use binstalk_downloader::{
    download::{DownloadError, ZipError},
    remote::{Error as RemoteError, HttpError, ReqwestError},
};
use cargo_toml::Error as CargoTomlError;
use compact_str::CompactString;
use miette::{Diagnostic, Report};
use thiserror::Error;
use tinytemplate::error::Error as TinyTemplateError;
use tokio::task;
use tracing::{error, warn};

#[derive(Debug, Error)]
#[error("crates.io API error for {crate_name}: {err}")]
pub struct CratesIoApiError {
    pub crate_name: CompactString,
    #[source]
    pub err: crates_io_api::Error,
}

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

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    ///
    /// - Code: `binstall::url_parse`
    /// - Exit: 65
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::url_parse))]
    UrlParse(#[from] url::ParseError),

    /// An error while unzipping a file.
    ///
    /// - Code: `binstall::unzip`
    /// - Exit: 66
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::unzip))]
    Unzip(#[from] ZipError),

    /// A rendering error in a template.
    ///
    /// - Code: `binstall::template`
    /// - Exit: 67
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::template))]
    Template(Box<TinyTemplateError>),

    /// A generic error from our HTTP client, reqwest.
    ///
    /// Errors resulting from HTTP fetches are handled with [`BinstallError::Http`] instead.
    ///
    /// - Code: `binstall::reqwest`
    /// - Exit: 68
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::reqwest))]
    Reqwest(#[from] ReqwestError),

    /// An HTTP request failed.
    ///
    /// This includes both connection/transport failures and when the HTTP status of the response
    /// is not as expected.
    ///
    /// - Code: `binstall::http`
    /// - Exit: 69
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::http))]
    Http(#[from] Box<HttpError>),

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
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::io))]
    Io(io::Error),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Code: `binstall::crates_io_api`
    /// - Exit: 76
    #[error(transparent)]
    #[diagnostic(
        severity(error),
        code(binstall::crates_io_api),
        help("Check that the crate name you provided is correct.\nYou can also search for a matching crate at: https://lib.rs/search?q={}", .0.crate_name)
    )]
    CratesIoApi(#[from] Box<CratesIoApiError>),

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
    #[error(transparent)]
    #[diagnostic(
        severity(error),
        code(binstall::cargo_manifest),
        help("If you used --manifest-path, check the Cargo.toml syntax.")
    )]
    CargoManifest(Box<CargoTomlError>),

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

    /// No available version matches the requirements.
    ///
    /// This may be the case when using the `--version` option.
    ///
    /// Note that using `--version 1.2.3` is interpreted as the requirement `=1.2.3`.
    ///
    /// - Code: `binstall::version::mismatch`
    /// - Exit: 82
    #[error("no version matching requirement '{req}'")]
    #[diagnostic(severity(error), code(binstall::version::mismatch))]
    VersionMismatch { req: semver::VersionReq },

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

    /// Bin file is not found.
    ///
    /// - Code: `binstall::binfile`
    /// - Exit: 88
    #[error("bin file {0} not found")]
    #[diagnostic(severity(error), code(binstall::binfile))]
    BinFileNotFound(PathBuf),

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

    /// bin-dir configuration provided generates source path outside
    /// of the temporary dir.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 91
    #[error(
        "bin-dir configuration provided generates source path outside of the temporary dir: {path}"
    )]
    #[diagnostic(severity(error), code(binstall::SourceFilePath))]
    InvalidSourceFilePath { path: PathBuf },

    /// bin-dir configuration provided generates empty source path.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 92
    #[error("bin-dir configuration provided generates empty source path")]
    #[diagnostic(severity(error), code(binstall::SourceFilePath))]
    EmptySourceFilePath,

    /// Fallback to `cargo-install` is disabled.
    ///
    /// - Code: `binstall::no_fallback_to_cargo_install`
    /// - Exit: 94
    #[error("Fallback to cargo-install is disabled")]
    #[diagnostic(severity(error), code(binstall::no_fallback_to_cargo_install))]
    NoFallbackToCargoInstall,

    /// A wrapped error providing the context of which crate the error is about.
    #[error(transparent)]
    #[diagnostic(transparent)]
    CrateContext(Box<CrateContextError>),
}

impl BinstallError {
    fn exit_number(&self) -> u8 {
        use BinstallError::*;
        let code: u8 = match self {
            TaskJoinError(_) => 17,
            UserAbort => 32,
            UrlParse(_) => 65,
            Unzip(_) => 66,
            Template(_) => 67,
            Reqwest(_) => 68,
            Http { .. } => 69,
            SubProcess { .. } => 70,
            Io(_) => 74,
            CratesIoApi { .. } => 76,
            CargoManifestPath => 77,
            CargoManifest { .. } => 78,
            VersionParse { .. } => 80,
            VersionMismatch { .. } => 82,
            SuperfluousVersionOption => 84,
            UnspecifiedBinaries => 86,
            NoViableTargets => 87,
            BinFileNotFound(_) => 88,
            CargoTomlMissingPackage(_) => 89,
            DuplicateSourceFilePath { .. } => 90,
            InvalidSourceFilePath { .. } => 91,
            EmptySourceFilePath => 92,
            NoFallbackToCargoInstall => 94,
            CrateContext(context) => context.err.exit_number(),
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
        Self::CrateContext(Box::new(CrateContextError {
            err: self,
            crate_name: crate_name.into(),
        }))
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
        if err.get_ref().is_some() {
            let kind = err.kind();

            let inner = err
                .into_inner()
                .expect("err.get_ref() returns Some, so err.into_inner() should also return Some");

            inner
                .downcast()
                .map(|b| *b)
                .unwrap_or_else(|err| BinstallError::Io(io::Error::new(kind, err)))
        } else {
            BinstallError::Io(err)
        }
    }
}

impl From<BinstallError> for io::Error {
    fn from(e: BinstallError) -> io::Error {
        match e {
            BinstallError::Io(io_error) => io_error,
            e => io::Error::new(io::ErrorKind::Other, e),
        }
    }
}

impl From<RemoteError> for BinstallError {
    fn from(e: RemoteError) -> Self {
        use RemoteError::*;

        match e {
            Reqwest(reqwest_error) => reqwest_error.into(),
            Http(http_error) => http_error.into(),
        }
    }
}

impl From<DownloadError> for BinstallError {
    fn from(e: DownloadError) -> Self {
        use DownloadError::*;

        match e {
            Unzip(zip_error) => zip_error.into(),
            Remote(remote_error) => remote_error.into(),
            Io(io_error) => io_error.into(),
            UserAbort => BinstallError::UserAbort,
        }
    }
}

impl From<TinyTemplateError> for BinstallError {
    fn from(e: TinyTemplateError) -> Self {
        BinstallError::Template(Box::new(e))
    }
}

impl From<CargoTomlError> for BinstallError {
    fn from(e: CargoTomlError) -> Self {
        BinstallError::CargoManifest(Box::new(e))
    }
}
