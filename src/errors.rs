use std::process::{ExitCode, Termination};

use log::warn;
use miette::{Report, Diagnostic};
use thiserror::Error;

/// Errors emitted by the library portion of cargo-binstall.
#[derive(Error, Diagnostic, Debug)]
#[diagnostic(url(docsrs))]
#[non_exhaustive]
pub enum BinstallError {
    /// The installation was cancelled by a user at a confirmation prompt.
    ///
    /// - Exit code: 32
    #[error("installation cancelled by user")]
    #[diagnostic(code(binstall::user_abort))]
    UserAbort,

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    ///
    /// - Exit code: 65
    #[error(transparent)]
    #[diagnostic(code(binstall::url_parse))]
    UrlParse(#[from] url::ParseError),

    /// An error while unzipping a file.
    ///
    /// - Exit code: 66
    #[error(transparent)]
    #[diagnostic(code(binstall::unzip))]
    Unzip(#[from] zip::result::ZipError),

    /// A rendering error in a template.
    ///
    /// - Exit code: 67
    #[error(transparent)]
    #[diagnostic(code(binstall::template))]
    Template(#[from] tinytemplate::error::Error),

    /// A generic error from our HTTP client, reqwest.
    ///
    /// Errors resulting from HTTP fetches are handled with [`BinstallError::Http`] instead.
    ///
    /// - Exit code: 68
    #[error(transparent)]
    #[diagnostic(code(binstall::reqwest))]
    Reqwest(#[from] reqwest::Error),

    /// An HTTP request failed.
    ///
    /// This includes both connection/transport failures and when the HTTP status of the response
    /// is not as expected.
    ///
    /// - Exit code: 69
    #[error("could not {method} {url}: {err}")]
    #[diagnostic(code(binstall::http))]
    Http {
        method: reqwest::Method,
        url: url::Url,
        #[source]
        err: reqwest::Error,
    },

    /// A generic I/O error.
    ///
    /// - Exit code: 74
    #[error(transparent)]
    #[diagnostic(code(binstall::io))]
    Io(#[from] std::io::Error),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Exit code: 76
    #[error("crates.io api error fetching crate information for '{crate_name}': {err}")]
    #[diagnostic(code(binstall::crates_io_api))]
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
    /// - Exit code: 78
    #[error(transparent)]
    #[diagnostic(code(binstall::cargo_manifest))]
    CargoManifest(#[from] cargo_toml::Error),

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
    ///
    /// - Exit code: 80
    #[error("version string '{v}' is not semver: {err}")]
    #[diagnostic(code(binstall::version::parse))]
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
    /// - Exit code: 81
    #[error("version requirement '{req}' is not semver: {err}")]
    #[diagnostic(code(binstall::version::requirement))]
    VersionReq {
        req: String,
        #[source]
        err: semver::Error,
    },

    /// No available version matches the requirements.
    ///
    /// This may be the case when using the `--version` option.
    ///
    /// Note that using `--version 1.2.3` is interpreted as the requirement `^1.2.3` as per
    /// Cargo.toml rules. If you want the exact version 1.2.3, use `--version '=1.2.3'`.
    ///
    /// - Exit code: 82
    #[error("no version matching requirement '{req}'")]
    #[diagnostic(code(binstall::version::mismatch))]
    VersionMismatch { req: semver::VersionReq },

    /// The crates.io API doesn't have manifest metadata for the given version.
    ///
    /// - Exit code: 83
    #[error("no crate information available for '{crate_name}' version '{v}'")]
    #[diagnostic(code(binstall::version::unavailable))]
    VersionUnavailable {
        crate_name: String,
        v: semver::Version,
    },
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
            eprintln!("{:?}", Report::new(self));
        }

        code
    }
}
