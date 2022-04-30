use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use cargo_toml::Product;
use log::debug;
use serde::Serialize;

use crate::{extract_file, PkgFmt, PkgMeta, Template};

pub struct BinFile {
    pub base_name: String,
    pub source: Option<PathBuf>,
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
        let source = if data.meta.pkg_fmt == PkgFmt::Bin {
            None
        } else {
            Some(PathBuf::from(ctx.render(&data.meta.bin_dir)?))
        };

        // Destination path is the install dir + base-name-version{.extension}
        let dest = data
            .install_path
            .join(ctx.render("{ bin }-v{ version }{ binary-ext }")?);

        // Link at install dir + base-name{.extension}
        let link = data
            .install_path
            .join(&ctx.render("{ bin }{ binary-ext }")?);

        Ok(Self {
            base_name,
            source,
            dest,
            link,
        })
    }

    pub fn preview_bin(&self) -> String {
        format!("{} ({})", self.base_name, self.dest.display())
    }

    pub fn preview_link(&self) -> String {
        format!(
            "{} ({} -> {})",
            self.base_name,
            self.link.display(),
            self.link_dest().display()
        )
    }

    pub fn is_binary_already_installed(&self) -> bool {
        self.dest.exists()
    }

    pub fn install_bin(&self, data: &bytes::Bytes, pkg_fmt: PkgFmt) -> Result<(), anyhow::Error> {
        debug!("Writing file to '{}'", self.dest.display());

        // Extract files
        let bin_file = if pkg_fmt != PkgFmt::Bin {
            extract_file(data, pkg_fmt, self.source.as_ref().unwrap().to_path_buf())?
        } else {
            data.to_vec()
        };

        if let Some(parent_dir) = self.dest.parent() {
            std::fs::create_dir_all(parent_dir)?;
        }

        // Will truncate existing file
        let mut file = File::create(&self.dest)?;
        file.write_all(&bin_file)?;
        file.flush().unwrap();

        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            debug!("Set permissions 755 on '{}'", self.dest.display());
            std::fs::set_permissions(&self.dest, std::fs::Permissions::from_mode(0o755))?;
        }

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), anyhow::Error> {
        // Remove existing symlink
        // TODO: check if existing symlink is correct
        if self.link.exists() {
            debug!("Remove link '{}'", self.link.display());
            std::fs::remove_file(&self.link)?;
        }

        let dest = self.link_dest();
        debug!(
            "Create link '{}' pointing to '{}'",
            self.link.display(),
            dest.display()
        );
        #[cfg(target_family = "unix")]
        std::os::unix::fs::symlink(dest, &self.link)?;
        #[cfg(target_family = "windows")]
        std::os::windows::fs::symlink_file(dest, &self.link)?;

        Ok(())
    }

    fn link_dest(&self) -> &Path {
        #[cfg(target_family = "unix")]
        {
            Path::new(self.dest.file_name().unwrap())
        }
        #[cfg(target_family = "windows")]
        {
            &self.dest
        }
    }
}

/// Data required to get bin paths
pub struct Data {
    pub name: String,
    pub target: String,
    pub version: String,
    pub repo: Option<String>,
    pub meta: PkgMeta,
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
