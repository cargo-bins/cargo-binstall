use std::{
    io,
    path::PathBuf,
    process::{ExitCode, ExitStatus, Termination},
};

use compact_str::CompactString;
use log::{error, warn};
use miette::{Diagnostic, Report};
use thiserror::Error;
use tokio::task;

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
    Unzip(#[from] zip::result::ZipError),

    /// A rendering error in a template.
    ///
    /// - Code: `binstall::template`
    /// - Exit: 67
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::template))]
    Template(#[from] tinytemplate::error::Error),

    /// A generic error from our HTTP client, reqwest.
    ///
    /// Errors resulting from HTTP fetches are handled with [`BinstallError::Http`] instead.
    ///
    /// - Code: `binstall::reqwest`
    /// - Exit: 68
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::reqwest))]
    Reqwest(#[from] reqwest::Error),

    /// An HTTP request failed.
    ///
    /// This includes both connection/transport failures and when the HTTP status of the response
    /// is not as expected.
    ///
    /// - Code: `binstall::http`
    /// - Exit: 69
    #[error("could not {method} {url}")]
    #[diagnostic(severity(error), code(binstall::http))]
    Http {
        method: reqwest::Method,
        url: url::Url,
        #[source]
        err: reqwest::Error,
    },

    /// A subprocess failed.
    ///
    /// This is often about cargo-install calls.
    ///
    /// - Code: `binstall::subprocess`
    /// - Exit: 70
    #[error("subprocess {command} errored with {status}")]
    #[diagnostic(severity(error), code(binstall::subprocess))]
    SubProcess { command: String, status: ExitStatus },

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
    #[error("crates.io API error")]
    #[diagnostic(
        severity(error),
        code(binstall::crates_io_api),
        help("Check that the crate name you provided is correct.\nYou can also search for a matching crate at: https://lib.rs/search?q={crate_name}")
    )]
    CratesIoApi {
        crate_name: CompactString,
        #[source]
        err: crates_io_api::Error,
    },

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
    CargoManifest(#[from] cargo_toml::Error),

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
    ///
    /// - Code: `binstall::version::parse`
    /// - Exit: 80
    #[error("version string '{v}' is not semver")]
    #[diagnostic(severity(error), code(binstall::version::parse))]
    VersionParse {
        v: CompactString,
        #[source]
        err: semver::Error,
    },

    /// A version requirement is not valid.
    ///
    /// This is usually provided via the `--version` option.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version requirement syntax; they
    /// may be slightly different from other semver requirements expressions implementations.
    ///
    /// - Code: `binstall::version::requirement`
    /// - Exit: 81
    #[error("version requirement '{req}' is not semver")]
    #[diagnostic(severity(error), code(binstall::version::requirement))]
    VersionReq {
        req: CompactString,
        #[source]
        err: semver::Error,
    },

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

    /// The crates.io API doesn't have manifest metadata for the given version.
    ///
    /// - Code: `binstall::version::unavailable`
    /// - Exit: 83
    #[error("no crate information available for '{crate_name}' version '{v}'")]
    #[diagnostic(severity(error), code(binstall::version::unavailable))]
    VersionUnavailable {
        crate_name: CompactString,
        v: semver::Version,
    },

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

    /// An override option is used when multiple packages are to be installed.
    ///
    /// This is raised when more than one package name is provided and any of:
    ///
    /// - `--version`
    /// - `--manifest-path`
    /// - `--bin-dir`
    /// - `--pkg-fmt`
    /// - `--pkg-url`
    ///
    /// is provided.
    ///
    /// - Code: `binstall::conflict::overrides`
    /// - Exit: 85
    #[error("override option used with multi package syntax")]
    #[diagnostic(
        severity(error),
        code(binstall::conflict::overrides),
        help("You cannot use --{option} and specify multiple packages at the same time. Do one or the other.")
    )]
    OverrideOptionUsedWithMultiInstall { option: &'static str },

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

    /// Invalid strategies configured.
    ///
    /// - Code: `binstall::strategies`
    /// - Exit: 93
    #[error("Invalid strategies configured: {0}")]
    #[diagnostic(severity(error), code(binstall::strategies))]
    InvalidStrategies(&'static &'static str),

    /// A wrapped error providing the context of which crate the error is about.
    #[error("for crate {crate_name}")]
    CrateContext {
        #[source]
        error: Box<BinstallError>,
        crate_name: CompactString,
    },
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
            VersionReq { .. } => 81,
            VersionMismatch { .. } => 82,
            VersionUnavailable { .. } => 83,
            SuperfluousVersionOption => 84,
            OverrideOptionUsedWithMultiInstall { .. } => 85,
            UnspecifiedBinaries => 86,
            NoViableTargets => 87,
            BinFileNotFound(_) => 88,
            CargoTomlMissingPackage(_) => 89,
            DuplicateSourceFilePath { .. } => 90,
            InvalidSourceFilePath { .. } => 91,
            EmptySourceFilePath => 92,
            InvalidStrategies(..) => 93,
            CrateContext { error, .. } => error.exit_number(),
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
        Self::CrateContext {
            error: Box::new(self),
            crate_name: crate_name.into(),
        }
    }
}

impl Termination for BinstallError {
    fn report(self) -> ExitCode {
        let code = self.exit_code();
        if let BinstallError::UserAbort = self {
            warn!("Installation cancelled");
        } else {
            error!("Fatal error:");
            eprintln!("{:?}", Report::new(self));
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
