use std::{
    borrow::Cow,
    fmt::{self, Write as _},
    str::FromStr,
};

use binstalk_types::maybe_owned::MaybeOwned;
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
    pub source: Source<'static>,
}

impl From<&CrateInfo> for CrateVersionSource {
    fn from(metadata: &CrateInfo) -> Self {
        use SourceType::*;

        let url = metadata.source.url.clone();

        super::CrateVersionSource {
            name: metadata.name.clone(),
            version: metadata.current_version.clone(),
            source: match metadata.source.source_type {
                Git => Source::Git(url),
                Path => Source::Path(url),
                Registry => Source::Registry(url),
                Sparse => Source::Sparse(url),
            },
        }
    }
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
pub enum Source<'a> {
    Git(MaybeOwned<'a, Url>),
    Path(MaybeOwned<'a, Url>),
    Registry(MaybeOwned<'a, Url>),
    Sparse(MaybeOwned<'a, Url>),
}

impl<'a> From<&'a CrateSource> for Source<'a> {
    fn from(source: &'a CrateSource) -> Self {
        use SourceType::*;

        let url = MaybeOwned::Borrowed(source.url.as_ref());

        match source.source_type {
            Git => Self::Git(url),
            Path => Self::Path(url),
            Registry => Self::Registry(url),
            Sparse => Self::Sparse(url),
        }
    }
}

impl FromStr for CrateVersionSource {
    type Err = CvsParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.splitn(3, ' ').collect::<Vec<_>>()[..] {
            [name, version, source] => {
                let version = version.parse()?;
                // Strip exactly one outer pair of parentheses, like Cargo's
                // `PackageId` parser: the url itself may end with `)`, e.g. a
                // `cargo install --path` from a directory named `dir (1)`.
                let source = source
                    .strip_prefix('(')
                    .and_then(|source| source.strip_suffix(')'))
                    .ok_or(CvsParseError::BadSource)?;
                let source = match source.splitn(2, '+').collect::<Vec<_>>()[..] {
                    ["git", url] => Source::Git(Url::parse(url)?.into()),
                    ["path", url] => Source::Path(Url::parse(url)?.into()),
                    ["registry", url] => Source::Registry(Url::parse(url)?.into()),
                    ["sparse", url] => Source::Sparse(Url::parse(url)?.into()),
                    [kind, arg] => {
                        return Err(CvsParseError::UnknownSourceType {
                            kind: kind.to_string().into_boxed_str(),
                            arg: arg.to_string().into_boxed_str(),
                        });
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
    #[error("Failed to parse url in cvs: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("Failed to parse version in cvs: {0}")]
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

impl fmt::Display for Source<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Git(url) => write!(f, "git+{url}"),
            Source::Path(url) => write!(f, "path+{url}"),
            Source::Registry(url) => write!(f, "registry+{url}"),
            Source::Sparse(url) => {
                let url = url.as_str();
                write!(f, "sparse+{url}")?;
                if url.ends_with("/") {
                    Ok(())
                } else {
                    f.write_char('/')
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(source: Source<'static>) {
        let cvs = CrateVersionSource {
            name: "cargo-watch".into(),
            version: Version::new(8, 5, 3),
            source,
        };
        let encoded = cvs.to_string();
        assert_eq!(
            CrateVersionSource::from_str(&encoded).unwrap(),
            cvs,
            "{encoded}"
        );
    }

    #[test]
    fn parens_in_url_roundtrip() {
        roundtrip(Source::Path(
            Url::parse("file:///work/dir(1)").unwrap().into(),
        ));
        roundtrip(Source::Path(
            Url::parse("file:///work/dir%20(1)").unwrap().into(),
        ));
        roundtrip(Source::Git(
            Url::parse("https://example.com/repo(x)?rev=(1)#abc")
                .unwrap()
                .into(),
        ));
    }

    #[test]
    fn parses_cargo_written_source_with_space_and_parens() {
        let cvs = CrateVersionSource::from_str("foo 0.1.0 (path+file:///work/dir%20(1))").unwrap();
        assert_eq!(
            cvs.source,
            Source::Path(Url::parse("file:///work/dir%20(1)").unwrap().into())
        );
    }

    #[test]
    fn rejects_unparenthesized_source() {
        for s in [
            "foo 0.1.0 path+file:///work/dir",
            "foo 0.1.0 (path+file:///work/dir",
            "foo 0.1.0 path+file:///work/dir)",
        ] {
            assert!(
                matches!(
                    CrateVersionSource::from_str(s),
                    Err(CvsParseError::BadSource)
                ),
                "{s}"
            );
        }
    }
}
