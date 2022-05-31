use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Diagnostic, Debug)]
pub enum BinstallError {
    #[error("installation cancelled by user")]
    #[diagnostic(code(binstall::user_abort))]
    UserAbort,

    #[error(transparent)]
    #[diagnostic(code(binstall::io))]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    #[diagnostic(code(binstall::url_parse))]
    UrlParse(#[from] url::ParseError),

    #[error(transparent)]
    #[diagnostic(code(binstall::reqwest))]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    #[diagnostic(code(binstall::template))]
    Template(#[from] tinytemplate::error::Error),

    #[error(transparent)]
    #[diagnostic(code(binstall::unzip))]
    Unzip(#[from] zip::result::ZipError),

    #[error(transparent)]
    #[diagnostic(code(binstall::cargo_manifest))]
    CargoManifest(#[from] cargo_toml::Error),

    #[error("crates.io api error fetching crate information for '{crate_name}': {err}")]
    #[diagnostic(code(binstall::crates_io_api))]
    CratesIoApi {
        crate_name: String,
        #[source]
        err: crates_io_api::Error,
    },

    #[error("version string '{v}' is not semver: {err}")]
    #[diagnostic(code(binstall::version::parse))]
    VersionParse {
        v: String,
        #[source]
        err: semver::Error,
    },

    #[error("version requirement '{req}' is not semver: {err}")]
    #[diagnostic(code(binstall::version::requirement))]
    VersionReq {
        req: String,
        #[source]
        err: semver::Error,
    },

    #[error("no version matching requirement '{req}'")]
    #[diagnostic(code(binstall::version::mismatch))]
    VersionMismatch { req: semver::VersionReq },

    #[error("no crate information available for '{crate_name}' version '{v}'")]
    #[diagnostic(code(binstall::version::unavailable))]
    VersionUnavailable {
        crate_name: String,
        v: semver::Version,
    },

    #[error("could not {method} {url}: {err}")]
    #[diagnostic(code(binstall::http))]
    Http {
        method: reqwest::Method,
        url: url::Url,
        #[source]
        err: reqwest::Error,
    },
}
