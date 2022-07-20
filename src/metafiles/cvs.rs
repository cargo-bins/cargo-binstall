use std::{fmt, str::FromStr};

use miette::Diagnostic;
use once_cell::sync::Lazy;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use url::Url;

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct CrateVersionSource {
    pub name: String,
    pub version: Version,
    pub source: Source,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub enum Source {
    Git(Url),
    Path(Url),
    Registry(Url),
}

impl Source {
    pub fn cratesio_registry() -> Source {
        static CRATESIO: Lazy<Url, fn() -> Url> =
            Lazy::new(|| url::Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

        Self::Registry(CRATESIO.clone())
    }
}

impl FromStr for CrateVersionSource {
    type Err = CvsParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.splitn(3, ' ').collect::<Vec<_>>()[..] {
            [name, version, source] => {
                let version = version.parse()?;
                let source = match source
                    .trim_matches(&['(', ')'][..])
                    .splitn(2, '+')
                    .collect::<Vec<_>>()[..]
                {
                    ["git", url] => Source::Git(Url::parse(url)?),
                    ["path", url] => Source::Path(Url::parse(url)?),
                    ["registry", url] => Source::Registry(Url::parse(url)?),
                    [kind, arg] => {
                        return Err(CvsParseError::UnknownSourceType {
                            kind: kind.to_string(),
                            arg: arg.to_string(),
                        })
                    }
                    _ => return Err(CvsParseError::BadSource),
                };
                Ok(Self {
                    name: name.to_string(),
                    version,
                    source,
                })
            }
            _ => Err(CvsParseError::BadFormat),
        }
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum CvsParseError {
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),

    #[error(transparent)]
    VersionParse(#[from] semver::Error),

    #[error("unknown source type {kind}+{arg}")]
    UnknownSourceType { kind: String, arg: String },

    #[error("bad source format")]
    BadSource,

    #[error("bad CVS format")]
    BadFormat,
}

impl fmt::Display for CrateVersionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            name,
            version,
            source,
        } = &self;
        write!(f, "{name} {version} ({source})")
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Git(url) => write!(f, "git+{url}"),
            Source::Path(url) => write!(f, "path+{url}"),
            Source::Registry(url) => write!(f, "registry+{url}"),
        }
    }
}

impl Serialize for CrateVersionSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CrateVersionSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_str(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
