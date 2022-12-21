use std::sync::Arc;

use compact_str::{CompactString, ToCompactString};
use semver::Version;
use tracing::{debug, info, warn};

use super::Options;
use crate::{
    bins,
    errors::BinstallError,
    fetchers::Fetcher,
    manifests::crate_info::{CrateInfo, CrateSource},
};

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

impl ResolutionFetch {
    pub fn install(self, opts: &Options) -> Result<Option<CrateInfo>, BinstallError> {
        if opts.dry_run {
            info!("Dry run, not proceeding");
            return Ok(None);
        }

        info!("Installing binaries...");
        for file in &self.bin_files {
            file.install_bin()?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &self.bin_files {
                file.install_link()?;
            }
        }

        Ok(Some(CrateInfo {
            name: self.name,
            version_req: self.version_req,
            current_version: self.new_version,
            source: CrateSource::cratesio_registry(),
            target: self.fetcher.target().to_compact_string(),
            bins: self
                .bin_files
                .into_iter()
                .map(|bin| bin.base_name)
                .collect(),
        }))
    }
}
