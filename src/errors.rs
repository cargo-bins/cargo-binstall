use std::process::{ExitCode, Termination};

use log::{error, warn};
use miette::{Diagnostic, Report};
use thiserror::Error;

/// Errors emitted by the library portion of cargo-binstall.
#[derive(Error, Diagnostic, Debug)]
#[diagnostic(url(docsrs))]
#[non_exhaustive]
pub enum BinstallError {
    /// The installation was cancelled by a user at a confirmation prompt.
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
    #[error("could not {method} {url}: {err}")]
    #[diagnostic(severity(error), code(binstall::http))]
    Http {
        method: reqwest::Method,
        url: url::Url,
        #[source]
        err: reqwest::Error,
    },

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::io))]
    Io(std::io::Error),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Code: `binstall::crates_io_api`
    /// - Exit: 76
    #[error("crates.io api error fetching crate information for '{crate_name}': {err}")]
    #[diagnostic(
        severity(error),
        code(binstall::crates_io_api),
        help("Check that the crate name you provided is correct.\nYou can also search for a matching crate at: https://lib.rs/search?q={crate_name}")
    )]
    CratesIoApi {
        crate_name: String,
        #[source]
        err: crates_io_api::Error,
    },

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
    #[error("version string '{v}' is not semver: {err}")]
    #[diagnostic(severity(error), code(binstall::version::parse))]
    VersionParse {
        v: String,
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
    #[error("version requirement '{req}' is not semver: {err}")]
    #[diagnostic(severity(error), code(binstall::version::requirement))]
    VersionReq {
        req: String,
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
        crate_name: String,
        v: semver::Version,
    },

    /// This occurs when you specified `--version` while also using
    /// form `$crate_name@$ver` tp specify version requirements.
    #[error("duplicate version requirements")]
    #[diagnostic(
        severity(error),
        code(binstall::version::requirement),
        help("Remove the `--version req` or simply use `$crate_name`")
    )]
    DuplicateVersionReq,

    /// This occurs when you specified `--manifest-path` while also
    /// specifing multiple crates to install.
    #[error("If you use --manifest-path, then you can only specify one crate to install")]
    #[diagnostic(
        severity(error),
        code(binstall::manifest_path),
        help("Remove the `--manifest-path` or only specify one `$crate_name`")
    )]
    ManifestPathConflictedWithBatchInstallation,
}

impl BinstallError {
    /// The recommended exit code for this error.
    ///
    /// This will never output:
    /// - 0 (success)
    /// - 1 and 2 (catchall and shell)
    /// - 16 (binstall errors not handled here)
    /// - 64 (generic error)
    pub fn exit_code(&self) -> ExitCode {
        use BinstallError::*;
        let code: u8 = match self {
            UserAbort => 32,
            UrlParse(_) => 65,
            Unzip(_) => 66,
            Template(_) => 67,
            Reqwest(_) => 68,
            Http { .. } => 69,
            Io(_) => 74,
            CratesIoApi { .. } => 76,
            CargoManifest { .. } => 78,
            VersionParse { .. } => 80,
            VersionReq { .. } => 81,
            VersionMismatch { .. } => 82,
            VersionUnavailable { .. } => 83,
            DuplicateVersionReq => 84,
            ManifestPathConflictedWithBatchInstallation => 85,
        };

        // reserved codes
        debug_assert!(code != 64 && code != 16 && code != 1 && code != 2 && code != 0);

        code.into()
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

impl From<std::io::Error> for BinstallError {
    fn from(err: std::io::Error) -> Self {
        if err.get_ref().is_some() {
            let kind = err.kind();

            let inner = err
                .into_inner()
                .expect("err.get_ref() returns Some, so err.into_inner() should also return Some");

            inner
                .downcast()
                .map(|b| *b)
                .unwrap_or_else(|err| BinstallError::Io(std::io::Error::new(kind, err)))
        } else {
            BinstallError::Io(err)
        }
    }
}
