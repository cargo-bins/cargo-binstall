use std::{path::PathBuf, sync::Arc};

use cargo_toml::{Package, Product};
use log::{debug, error, info, warn};
use miette::{miette, Result};

use super::{Context, Options};
use crate::{
    bins,
    fetchers::{Data, Fetcher, GhCrateMeta, MultiFetcher, QuickInstall},
    *,
};

pub enum Resolution {
    Fetch {
        fetcher: Arc<dyn Fetcher>,
        package: Package<Meta>,
        name: String,
        version: String,
        bin_path: PathBuf,
        bin_files: Vec<bins::BinFile>,
    },
    Source {
        package: Package<Meta>,
    },
}

impl Resolution {
    pub fn name(&self) -> &str {
        match self {
            Resolution::Fetch { package, .. } | Resolution::Source { package } => {
                package.name.as_ref()
            }
        }
    }

    fn print(&self, opts: &Options) {
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

                if fetcher.is_third_party() {
                    warn!(
                        "The package will be downloaded from third-party source {}",
                        fetcher.source_name()
                    );
                } else {
                    info!(
                        "The package will be downloaded from {}",
                        fetcher.source_name()
                    );
                }

                info!("This will install the following binaries:");
                for file in bin_files {
                    info!("  - {}", file.main_preview());
                }

                if opts.versioned {
                    for file in bin_files {
                        info!("  - {}", file.versioned_preview());
                    }
                }
            }
            Resolution::Source { .. } => {
                warn!("The package will be installed from source (with cargo)",)
            }
        }
    }
}

pub async fn resolve(ctx: Context, crate_name: CrateName) -> Result<Resolution> {
    info!("Installing package: '{}'", crate_name);
    let Context {
        opts,
        temp_dir,
        install_path,
        client,
        crates_io_api_client,
        ..
    } = ctx;

    let mut version = match (&crate_name.version, &opts.version) {
        (Some(version), None) => version.to_string(),
        (None, Some(version)) => version.to_string(),
        (Some(_), Some(_)) => Err(BinstallError::SuperfluousVersionOption)?,
        (None, None) => "*".to_string(),
    };

    // Treat 0.1.2 as =0.1.2
    if version
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        version.insert(0, '=');
    }

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let manifest = match opts.manifest_path.clone() {
        Some(manifest_path) => load_manifest_path(manifest_path)?,
        None => {
            fetch_crate_cratesio(&client, &crates_io_api_client, &crate_name.name, &version).await?
        }
    };

    let package = manifest.package.unwrap();

    let (mut meta, binaries) = (
        package
            .metadata
            .as_ref()
            .and_then(|m| m.binstall.clone())
            .unwrap_or_default(),
        manifest.bin,
    );

    let mut fetchers = MultiFetcher::default();

    let desired_targets = opts.desired_targets.get().await;

    for target in desired_targets {
        debug!("Building metadata for target: {target}");
        let mut target_meta = meta.clone();

        // Merge any overrides
        if let Some(o) = target_meta.overrides.get(target).cloned() {
            target_meta.merge(&o);
        }

        target_meta.merge(&opts.cli_overrides);
        debug!("Found metadata: {target_meta:?}");

        let fetcher_data = Data {
            name: package.name.clone(),
            target: target.clone(),
            version: package.version.clone(),
            repo: package.repository.clone(),
            meta: target_meta,
        };

        fetchers.add(GhCrateMeta::new(&client, &fetcher_data).await);
        fetchers.add(QuickInstall::new(&client, &fetcher_data).await);
    }

    let resolution = match fetchers.first_available().await {
        Some(fetcher) => {
            // Build final metadata
            let fetcher_target = fetcher.target();
            if let Some(o) = meta.overrides.get(&fetcher_target.to_owned()).cloned() {
                meta.merge(&o);
            }
            meta.merge(&opts.cli_overrides);

            // Generate temporary binary path
            let bin_path = temp_dir.join(format!("bin-{}", crate_name.name));
            debug!("Using temporary binary path: {}", bin_path.display());

            let bin_files = collect_bin_files(
                fetcher.as_ref(),
                &package,
                meta,
                binaries,
                bin_path.clone(),
                install_path.to_path_buf(),
            )?;

            Resolution::Fetch {
                fetcher,
                package,
                name: crate_name.name,
                version,
                bin_path,
                bin_files,
            }
        }
        None => Resolution::Source { package },
    };

    resolution.print(&opts);

    Ok(resolution)
}

fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package: &Package<Meta>,
    mut meta: PkgMeta,
    binaries: Vec<Product>,
    bin_path: PathBuf,
    install_path: PathBuf,
) -> Result<Vec<bins::BinFile>> {
    // Update meta
    if fetcher.source_name() == "QuickInstall" {
        // TODO: less of a hack?
        meta.bin_dir = "{ bin }{ binary-ext }".to_string();
    }

    // Check binaries
    if binaries.is_empty() {
        error!("No binaries specified (or inferred from file system)");
        return Err(miette!(
            "No binaries specified (or inferred from file system)"
        ));
    }

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: package.name.clone(),
        target: fetcher.target().to_string(),
        version: package.version.clone(),
        repo: package.repository.clone(),
        meta,
        bin_path,
        install_path,
    };

    // Create bin_files
    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p))
        .collect::<Result<Vec<_>, BinstallError>>()?;

    Ok(bin_files)
}
