use compact_str::CompactString;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::binstall::MetaData;

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
