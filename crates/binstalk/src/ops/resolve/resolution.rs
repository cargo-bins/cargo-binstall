use std::sync::Arc;

use compact_str::CompactString;
use semver::Version;
use tracing::{debug, info, warn};

use super::Options;
use crate::{bins, fetchers::Fetcher};

pub struct ResolutionFetch {
    pub fetcher: Arc<dyn Fetcher>,
    pub new_version: Version,
    pub name: CompactString,
    pub version_req: CompactString,
    pub bin_files: Vec<bins::BinFile>,
}

pub struct ResolutionInstallFromSource {
    pub name: CompactString,
    pub version: CompactString,
}

pub enum Resolution {
    Fetch(ResolutionFetch),
    InstallFromSource(ResolutionInstallFromSource),
    AlreadyUpToDate,
}
impl Resolution {
    pub fn print(&self, opts: &Options) {
        match self {
            Resolution::Fetch(ResolutionFetch {
                fetcher,
                bin_files,
                name,
                new_version,
                ..
            }) => {
                let fetcher_target = fetcher.target();
                // Prompt user for confirmation
                debug!(
                    "Found a binary install source: {} ({fetcher_target})",
                    fetcher.source_name()
                );

                warn!(
                    "The package {name} v{new_version} will be downloaded from {}{}",
                    if fetcher.is_third_party() {
                        "third-party source "
                    } else {
                        ""
                    },
                    fetcher.source_name()
                );

                info!("This will install the following binaries:");
                for file in bin_files {
                    info!("  - {}", file.preview_bin());
                }

                if !opts.no_symlinks {
                    info!("And create (or update) the following symlinks:");
                    for file in bin_files {
                        info!("  - {}", file.preview_link());
                    }
                }
            }
            Resolution::InstallFromSource(ResolutionInstallFromSource { name, version }) => {
                warn!("The package {name} v{version} will be installed from source (with cargo)",)
            }
            Resolution::AlreadyUpToDate => (),
        }
    }
}
