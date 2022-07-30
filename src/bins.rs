use std::{
    fs,
    path::{Path, PathBuf},
};

use cargo_toml::Product;
use compact_str::CompactString;
use log::{debug, info};
use serde::Serialize;

use crate::{BinstallError, PkgFmt, PkgMeta, Template};

pub struct BinFile {
    pub base_name: CompactString,
    pub source: PathBuf,
    pub versioned: PathBuf,
    pub main: PathBuf,
}

impl BinFile {
    pub fn from_product(data: &Data, product: &Product) -> Result<Self, BinstallError> {
        let base_name = CompactString::from(product.name.clone().unwrap());

        let binary_ext = if data.target.contains("windows") {
            ".exe"
        } else {
            ""
        };

        let ctx = Context {
            name: &data.name,
            repo: data.repo.as_ref().map(|s| &s[..]),
            target: &data.target,
            version: &data.version,
            bin: &base_name,
            format: binary_ext,
            binary_ext,
        };

        // Generate install paths
        // Source path is the download dir + the generated binary path
        let source_file_path = ctx.render(&data.meta.bin_dir)?;
        let source = if data.meta.pkg_fmt == PkgFmt::Bin {
            data.bin_path.clone()
        } else {
            data.bin_path.join(&source_file_path)
        };

        // Versioned path is the install dir + base-name-version{.extension}
        let verd_file_path = ctx.render("{ bin }-v{ version }{ binary-ext }")?;
        let versioned = data.install_path.join(verd_file_path);

        // Main file at install dir + base-name{.extension}
        let main = data
            .install_path
            .join(&ctx.render("{ bin }{ binary-ext }")?);

        Ok(Self {
            base_name,
            source,
            versioned,
            main,
        })
    }

    pub fn main_filename(&self) -> String {
        self.main.file_name().unwrap().to_string_lossy().into()
    }

    pub fn versioned_filename(&self) -> String {
        self.versioned.file_name().unwrap().to_string_lossy().into()
    }

    pub fn versioned_preview(&self) -> String {
        format!(
            "{} ({} -> {})",
            self.base_name,
            self.source.file_name().unwrap().to_string_lossy(),
            self.versioned.display()
        )
    }

    pub fn main_preview(&self) -> String {
        format!(
            "{} ({} -> {})",
            self.base_name,
            self.source.file_name().unwrap().to_string_lossy(),
            self.main.display(),
        )
    }

    pub fn install_only_main(&self) -> Result<(), BinstallError> {
        info!("Install: {}", self.main_preview());
        if install_move(&self.source, &self.main).is_err() {
            try_remove(&self.main);
            install_copy(&self.source, &self.main)?;
        }

        Ok(())
    }

    pub fn install_versioned(&self) -> Result<(), BinstallError> {
        info!("Install versioned: {}", self.versioned_preview());
        if install_move(&self.source, &self.versioned).is_err() {
            try_remove(&self.versioned);
            install_copy(&self.source, &self.versioned)?;
        }

        info!("Install main: {}", self.main_preview());
        try_remove(&self.main);
        install_link_or_copy(&self.versioned, &self.main)?;

        Ok(())
    }
}

/// Data required to get bin paths
pub struct Data {
    pub name: String,
    pub target: String,
    pub version: String,
    pub repo: Option<String>,
    pub meta: PkgMeta,
    pub bin_path: PathBuf,
    pub install_path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
struct Context<'c> {
    pub name: &'c str,
    pub repo: Option<&'c str>,
    pub target: &'c str,
    pub version: &'c str,
    pub bin: &'c str,

    /// Soft-deprecated alias for binary-ext
    pub format: &'c str,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    #[serde(rename = "binary-ext")]
    pub binary_ext: &'c str,
}

impl<'c> Template for Context<'c> {}

fn try_remove(path: &Path) {
    debug!("Try to remove {}", path.display());
    if let Err(err) = fs::remove_file(&path) {
        debug!("Removing destination errored: {}", err);
    }
}

fn install_move(src: &Path, dst: &Path) -> Result<(), BinstallError> {
    debug!(
        "Installing file (move) from '{}' to '{}'",
        src.display(),
        dst.display()
    );

    fs::rename(src, dst)?;
    Ok(())
}

fn install_copy(src: &Path, dst: &Path) -> Result<(), BinstallError> {
    debug!(
        "Installing file (copy) from '{}' to '{}'",
        src.display(),
        dst.display()
    );

    #[cfg(feature = "reflink")]
    {
        debug!("Reflink copy or fallback");
        reflink::reflink_or_copy(src, dst)?;
    }
    #[cfg(not(feature = "reflink"))]
    {
        debug!("Copy file");
        fs::copy(src, dst)?;
    }

    #[cfg(unix)]
    {
        debug!("Copy permissions");
        let permissions = fs::File::open(src)?.metadata()?.permissions();
        fs::File::open(dst)?.set_permissions(permissions)?;
    }

    Ok(())
}

fn install_link_or_copy(src: &Path, dst: &Path) -> Result<(), BinstallError> {
    debug!(
        "Installing file (link or copy) from '{}' to '{}'",
        src.display(),
        dst.display()
    );

    #[cfg(unix)]
    {
        debug!("Try to symlink");
        if let Err(err) = std::os::unix::fs::symlink(&src, &dst) {
            debug!("Symlinking errored: {}", err);
        } else {
            return Ok(());
        }
    }

    install_copy(src, dst)
}
