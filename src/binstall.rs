use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    process,
    sync::Arc,
};

use cargo_toml::{Package, Product};
use log::{debug, error, info, warn};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use reqwest::Client;
use tokio::{process::Command, task::block_in_place};

use super::{
    bins,
    fetchers::{Data, Fetcher, GhCrateMeta, MultiFetcher, QuickInstall},
    *,
};

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub version: Option<String>,
    pub manifest_path: Option<PathBuf>,
}

pub enum Resolution {
    Fetch {
        fetcher: Arc<dyn Fetcher>,
        package: Package<Meta>,
        name: String,
        version: String,
        bin_path: PathBuf,
        bin_files: Vec<bins::BinFile>,
    },
    InstallFromSource {
        package: Package<Meta>,
    },
}
impl Resolution {
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
        }
    }
}

pub async fn resolve(
    opts: Arc<Options>,
    crate_name: CrateName,
    desired_targets: DesiredTargets,
    cli_overrides: Arc<PkgOverride>,
    temp_dir: Arc<Path>,
    install_path: Arc<Path>,
    client: Client,
) -> Result<Resolution> {
    info!("Installing package: '{}'", crate_name);

    let mut version = match (&crate_name.version, &opts.version) {
        (Some(version), None) => version.to_string(),
        (None, Some(version)) => version.to_string(),
        (Some(_), Some(_)) => Err(BinstallError::DuplicateVersionReq)?,
        (None, None) => "*".to_string(),
    };

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
        Some(manifest_path) => load_manifest_path(manifest_path.join("Cargo.toml"))?,
        None => fetch_crate_cratesio(&client, &crate_name.name, &version).await?,
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

    let desired_targets = desired_targets.get().await;

    for target in desired_targets {
        debug!("Building metadata for target: {target}");
        let mut target_meta = meta.clone();

        // Merge any overrides
        if let Some(o) = target_meta.overrides.get(target).cloned() {
            target_meta.merge(&o);
        }

        target_meta.merge(&cli_overrides);
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
            meta.merge(&cli_overrides);

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
        None => Resolution::InstallFromSource { package },
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

pub async fn install(
    resolution: Resolution,
    opts: Arc<Options>,
    desired_targets: DesiredTargets,
    jobserver_client: jobserver::Client,
) -> Result<()> {
    match resolution {
        Resolution::Fetch {
            fetcher,
            package,
            name,
            version,
            bin_path,
            bin_files,
        } => {
            let cvs = metafiles::CrateVersionSource {
                name,
                version: package.version.parse().into_diagnostic()?,
                source: metafiles::Source::cratesio_registry(),
            };

            install_from_package(fetcher, opts, cvs, version, bin_path, bin_files).await
        }
        Resolution::InstallFromSource { package } => {
            let desired_targets = desired_targets.get().await;
            let target = desired_targets
                .first()
                .ok_or_else(|| miette!("No viable targets found, try with `--targets`"))?;

            if !opts.dry_run {
                install_from_source(package, target, jobserver_client).await
            } else {
                info!(
                    "Dry-run: running `cargo install {} --version {} --target {target}`",
                    package.name, package.version
                );
                Ok(())
            }
        }
    }
}

async fn install_from_package(
    fetcher: Arc<dyn Fetcher>,
    opts: Arc<Options>,
    cvs: metafiles::CrateVersionSource,
    version: String,
    bin_path: PathBuf,
    bin_files: Vec<bins::BinFile>,
) -> Result<()> {
    // Download package
    if opts.dry_run {
        info!("Dry run, not downloading package");
    } else {
        fetcher.fetch_and_extract(&bin_path).await?;
    }

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone()).flatten() {
            debug!("Found public key: {pub_key}");

            // Generate signature file URL
            let mut sig_ctx = ctx.clone();
            sig_ctx.format = "sig".to_string();
            let sig_url = sig_ctx.render(&pkg_url)?;

            debug!("Fetching signature file: {sig_url}");

            // Download signature file
            let sig_path = temp_dir.join(format!("{pkg_name}.sig"));
            download(&sig_url, &sig_path).await?;

            // TODO: do the signature check
            unimplemented!()
        } else {
            warn!("No public key found, package signature could not be validated");
        }
    }

    if opts.dry_run {
        info!("Dry run, not proceeding");
        return Ok(());
    }

    info!("Installing binaries...");
    block_in_place(|| {
        for file in &bin_files {
            file.install_bin()?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &bin_files {
                file.install_link()?;
            }
        }

        let bins: BTreeSet<String> = bin_files.into_iter().map(|bin| bin.base_name).collect();

        {
            debug!("Writing .crates.toml");
            let mut c1 = metafiles::v1::CratesToml::load().unwrap_or_default();
            c1.insert(cvs.clone(), bins.clone());
            c1.write()?;
        }

        {
            debug!("Writing .crates2.json");
            let mut c2 = metafiles::v2::Crates2Json::load().unwrap_or_default();
            c2.insert(
                cvs,
                metafiles::v2::CrateInfo {
                    version_req: Some(version),
                    bins,
                    profile: "release".into(),
                    target: fetcher.target().to_string(),
                    rustc: format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                    ..Default::default()
                },
            );
            c2.write()?;
        }

        Ok(())
    })
}

async fn install_from_source(
    package: Package<Meta>,
    target: &str,
    jobserver_client: jobserver::Client,
) -> Result<()> {
    debug!(
        "Running `cargo install {} --version {} --target {target}`",
        package.name, package.version
    );
    let mut command = process::Command::new("cargo");
    jobserver_client.configure(&mut command);

    let mut child = Command::from(command)
        .arg("install")
        .arg(package.name)
        .arg("--version")
        .arg(package.version)
        .arg("--target")
        .arg(&*target)
        .spawn()
        .into_diagnostic()
        .wrap_err("Spawning cargo install failed.")?;
    debug!("Spawned command pid={:?}", child.id());

    let status = child
        .wait()
        .await
        .into_diagnostic()
        .wrap_err("Running cargo install failed.")?;
    if status.success() {
        info!("Cargo finished successfully");
        Ok(())
    } else {
        error!("Cargo errored! {status:?}");
        Err(miette!("Cargo install error"))
    }
}
