use std::{borrow::Cow, fmt, str::FromStr};

use binstalk_types::crate_info::cratesio_url;
use compact_str::CompactString;
use miette::Diagnostic;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use url::Url;

use crate::crate_info::{CrateInfo, CrateSource, SourceType};

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub struct CrateVersionSource {
    pub name: CompactString,
    pub version: Version,
    pub source: Source,
}

impl From<&CrateInfo> for CrateVersionSource {
    fn from(metadata: &CrateInfo) -> Self {
        super::CrateVersionSource {
            name: metadata.name.clone(),
            version: metadata.current_version.clone(),
            source: Source::from(&metadata.source),
        }
    }
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub enum Source {
    Git(Url),
    Path(Url),
    Registry(Url),
}

impl Source {
    pub fn cratesio_registry() -> Source {
        Self::Registry(cratesio_url().clone())
    }
}

impl From<&CrateSource> for Source {
    fn from(source: &CrateSource) -> Self {
        use SourceType::*;

        let url = source.url.clone();

        match source.source_type {
            Git => Self::Git(url),
            Path => Self::Path(url),
            Registry => Self::Registry(url),
        }
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
                            kind: kind.to_string().into_boxed_str(),
                            arg: arg.to_string().into_boxed_str(),
                        })
                    }
                    _ => return Err(CvsParseError::BadSource),
                };
                Ok(Self {
                    name: name.into(),
                    version,
                    source,
                })
            }
            _ => Err(CvsParseError::BadFormat),
        }
    }
}

#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum CvsParseError {
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),

    #[error(transparent)]
    VersionParse(#[from] semver::Error),

    #[error("unknown source type {kind}+{arg}")]
    UnknownSourceType { kind: Box<str>, arg: Box<str> },

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
        let s = Cow::<'_, str>::deserialize(deserializer)?;
        Self::from_str(&s).map_err(serde::de::Error::custom)
    }
}
