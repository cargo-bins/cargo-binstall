use std::sync::Arc;

use compact_str::CompactString;
use semver::Version;
use tracing::{debug, info, warn};

use super::Options;
use crate::{bins, fetchers::Fetcher};

pub enum Resolution {
    Fetch {
        fetcher: Arc<dyn Fetcher>,
        new_version: Version,
        name: CompactString,
        version_req: CompactString,
        bin_files: Vec<bins::BinFile>,
    },
    InstallFromSource {
        name: CompactString,
        version: CompactString,
    },
    AlreadyUpToDate,
}
impl Resolution {
    pub(super) fn print(&self, opts: &Options) {
        match self {
            Resolution::Fetch {
                fetcher, bin_files, ..
            } => {
                let fetcher_target = fetcher.target();
                // Prompt user for confirmation
                debug!(
                    "Found a binary install source: {} ({fetcher_target})",
                    fetcher.source_name()
                );

                warn!(
                    "The package will be downloaded from {}{}",
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
            Resolution::InstallFromSource { .. } => {
                warn!("The package will be installed from source (with cargo)",)
            }
            Resolution::AlreadyUpToDate => (),
        }
    }
}
