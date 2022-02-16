use std::path::PathBuf;

use cargo_toml::Product;
use serde::Serialize;

use crate::{PkgFmt, PkgMeta, Template};

pub struct BinFile {
    pub base_name: String,
    pub source: PathBuf,
    pub dest: PathBuf,
    pub link: PathBuf,
}

impl BinFile {
    pub fn from_product(data: &Data, product: &Product) -> Result<Self, anyhow::Error> {
        let base_name = product.name.clone().unwrap();

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

        // Destination path is the install dir + base-name-version{.extension}
        let dest_file_path = ctx.render("{ bin }-v{ version }{ binary-ext }")?;
        let dest = data.install_path.join(dest_file_path);

        // Link at install dir + base name
        let link = data.install_path.join(&base_name);

        Ok(Self {
            base_name,
            source,
            dest,
            link,
        })
    }

    pub fn preview_bin(&self) -> String {
        format!(
            "{} ({} -> {})",
            self.base_name,
            self.source.file_name().unwrap().to_string_lossy(),
            self.dest.display()
        )
    }

    pub fn preview_link(&self) -> String {
        format!(
            "{} ({} -> {})",
            self.base_name,
            self.dest.display(),
            self.link.display()
        )
    }

    pub fn install_bin(&self) -> Result<(), anyhow::Error> {
        // TODO: check if file already exists
        std::fs::copy(&self.source, &self.dest)?;

        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dest, std::fs::Permissions::from_mode(0o755))?;
        }

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), anyhow::Error> {
        // Remove existing symlink
        // TODO: check if existing symlink is correct
        if self.link.exists() {
            std::fs::remove_file(&self.link)?;
        }

        #[cfg(target_family = "unix")]
        std::os::unix::fs::symlink(&self.dest, &self.link)?;
        #[cfg(target_family = "windows")]
        std::os::windows::fs::symlink_file(&self.dest, &self.link)?;

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
