use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use miette::Diagnostic;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use url::Url;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CratesTomlRaw {
    #[serde(default)]
    pub v1: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct CratesToml(BTreeMap<CrateVersionSource, BTreeSet<String>>);

impl CratesToml {
    pub fn default_path() -> Result<PathBuf, CratesTomlParseError> {
        Ok(home::cargo_home()?.join(".crates.toml"))
    }

    pub async fn load() -> Result<Self, CratesTomlParseError> {
        Self::load_from_path(Self::default_path()?).await
    }

    pub async fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CratesTomlParseError> {
        let file = fs::read_to_string(path).await?;
        Self::from_str(&file)
    }

    pub fn insert(&mut self, cvs: CrateVersionSource, bins: impl Iterator<Item = String>) {
        self.0.insert(cvs, bins.collect());
    }

    pub async fn write(&self) -> Result<(), CratesTomlParseError> {
        self.write_to_path(Self::default_path()?).await
    }

    pub async fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), CratesTomlParseError> {
        let raw = CratesTomlRaw {
            v1: self
                .0
                .iter()
                .map(|(cvs, bins)| (cvs.to_string(), bins.clone()))
                .collect(),
        };

        fs::write(path, &toml::to_vec(&raw)?).await?;
        Ok(())
    }
}

impl FromStr for CratesToml {
    type Err = CratesTomlParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw: CratesTomlRaw = toml::from_str(s).unwrap();

        Ok(Self(
            raw.v1
                .into_iter()
                .map(|(name, bins)| CrateVersionSource::from_str(&name).map(|cvs| (cvs, bins)))
                .collect::<Result<_, _>>()?,
        ))
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum CratesTomlParseError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    TomlParse(#[from] toml::de::Error),

    #[error(transparent)]
    TomlWrite(#[from] toml::ser::Error),

    #[error(transparent)]
    CvsParse(#[from] CvsParseError),
}

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
