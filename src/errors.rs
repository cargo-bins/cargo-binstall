use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Diagnostic, Debug)]
#[diagnostic(url(docsrs))]
pub enum BinstallError {
    /// The installation was cancelled by a user at a confirmation prompt.
    #[error("installation cancelled by user")]
    #[diagnostic(code(binstall::user_abort))]
    UserAbort,

    /// A generic I/O error.
    #[error(transparent)]
    #[diagnostic(code(binstall::io))]
    Io(#[from] std::io::Error),

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    #[error(transparent)]
    #[diagnostic(code(binstall::url_parse))]
    UrlParse(#[from] url::ParseError),

    /// A generic error from our HTTP client, reqwest.
    ///
    /// Errors resulting from HTTP fetches are handled with [`BinstallError::Http`] instead.
    #[error(transparent)]
    #[diagnostic(code(binstall::reqwest))]
    Reqwest(#[from] reqwest::Error),

    /// A rendering error in a template.
    #[error(transparent)]
    #[diagnostic(code(binstall::template))]
    Template(#[from] tinytemplate::error::Error),

    /// An error while unzipping a file.
    #[error(transparent)]
    #[diagnostic(code(binstall::unzip))]
    Unzip(#[from] zip::result::ZipError),

    /// A parsing or validation error in a cargo manifest.
    ///
    /// This should be rare, as manifests are generally fetched from crates.io, which does its own
    /// validation upstream. The most common failure will therefore be for direct repository access
    /// and with the `--manifest-path` option.
    #[error(transparent)]
    #[diagnostic(code(binstall::cargo_manifest))]
    CargoManifest(#[from] cargo_toml::Error),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    #[error("crates.io api error fetching crate information for '{crate_name}': {err}")]
    #[diagnostic(code(binstall::crates_io_api))]
    CratesIoApi {
        crate_name: String,
        #[source]
        err: crates_io_api::Error,
    },

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
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
    #[error("no version matching requirement '{req}'")]
    #[diagnostic(code(binstall::version::mismatch))]
    VersionMismatch { req: semver::VersionReq },

    /// The crates.io API doesn't have manifest metadata for the given version.
    #[error("no crate information available for '{crate_name}' version '{v}'")]
    #[diagnostic(code(binstall::version::unavailable))]
    VersionUnavailable {
        crate_name: String,
        v: semver::Version,
    },

    /// An HTTP request failed.
    ///
    /// This includes both connection/transport failures and when the HTTP status of the response
    /// is not as expected.
    #[error("could not {method} {url}: {err}")]
    #[diagnostic(code(binstall::http))]
    Http {
        method: reqwest::Method,
        url: url::Url,
        #[source]
        err: reqwest::Error,
    },
}
