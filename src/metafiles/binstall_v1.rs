use std::{
    fs,
    io::{self, Write},
    iter::IntoIterator,
    path::Path,
};

use compact_str::CompactString;
use miette::Diagnostic;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::binstall::MetaData;
use crate::FileLock;

#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    pub name: CompactString,
    pub version_req: CompactString,
    pub current_version: Version,
    pub source: Source,
    pub target: CompactString,
    pub bins: Vec<CompactString>,
}
impl Entry {
    pub fn new(metadata: MetaData) -> Self {
        let MetaData {
            bins,
            cvs:
                super::CrateVersionSource {
                    name,
                    version,
                    source,
                },
            version_req,
            target,
        } = metadata;

        Self {
            name: name.into(),
            version_req: version_req.into(),
            current_version: version,
            source: source.into(),
            target: target.into(),
            bins,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Source {
    source_type: CompactString,
    url: Url,
}

impl From<super::Source> for Source {
    fn from(src: super::Source) -> Self {
        use super::Source::*;

        match src {
            Git(url) => Source {
                source_type: "Git".into(),
                url,
            },
            Path(url) => Source {
                source_type: "Path".into(),
                url,
            },
            Registry(url) => Source {
                source_type: "Registry".into(),
                url,
            },
        }
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    SerdeJsonParse(#[from] serde_json::Error),
}

pub fn append_to_path<Iter>(path: impl AsRef<Path>, iter: Iter) -> Result<(), Error>
where
    Iter: IntoIterator<Item = Entry>,
{
    let file = FileLock::new_exclusive(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?,
    )?;

    let writer = io::BufWriter::with_capacity(512, file);

    let mut ser = serde_json::Serializer::new(writer);

    for item in iter {
        item.serialize(&mut ser)?;
    }

    ser.into_inner().flush()?;

    Ok(())
}
