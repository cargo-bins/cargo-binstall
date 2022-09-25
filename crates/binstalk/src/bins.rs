use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use cargo_toml::Product;
use compact_str::CompactString;
use log::debug;
use serde::Serialize;
use tinytemplate::TinyTemplate;

use crate::{
    errors::BinstallError,
    fs::{atomic_install, atomic_symlink_file},
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

pub struct BinFile {
    pub base_name: CompactString,
    pub source: PathBuf,
    pub dest: PathBuf,
    pub link: PathBuf,
}

impl BinFile {
    /// Must be called after the archive is downloaded and extracted.
    /// This function might uses blocking I/O.
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
        let source_file_path = if let Some(bin_dir) = &data.meta.bin_dir {
            ctx.render(bin_dir)?
        } else {
            let name = ctx.name;
            let target = ctx.target;
            let version = ctx.version;
            let bin = ctx.bin;

            // Make sure to update
            // fetchers::gh_crate_meta::hosting::{FULL_FILENAMES,
            // NOVERSION_FILENAMES} if you update this array.
            let possible_dirs = [
                format!("{name}-{target}-v{version}"),
                format!("{name}-{target}-{version}"),
                format!("{name}-{version}-{target}"),
                format!("{name}-v{version}-{target}"),
                format!("{name}-{target}"),
                name.to_string(),
            ];

            let dir = possible_dirs
                .into_iter()
                .find(|dirname| Path::new(dirname).is_dir())
                .map(Cow::Owned)
                // Fallback to no dir
                .unwrap_or_else(|| Cow::Borrowed("."));

            debug!("Using dir = {dir}");

            format!("{dir}/{bin}{binary_ext}")
        };

        let source = if data.meta.pkg_fmt == Some(PkgFmt::Bin) {
            data.bin_path.clone()
        } else {
            data.bin_path.join(&source_file_path)
        };

        // Destination path is the install dir + base-name-version{.extension}
        let dest_file_path = ctx.render("{ bin }-v{ version }{ binary-ext }")?;
        let dest = data.install_path.join(dest_file_path);

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
            self.link.display(),
            self.link_dest().display()
        )
    }

    /// Return `Ok` if the source exists, otherwise `Err`.
    pub fn check_source_exists(&self) -> Result<(), BinstallError> {
        if !self.source.try_exists()? {
            Err(BinstallError::BinFileNotFound(self.source.clone()))
        } else {
            Ok(())
        }
    }

    pub fn install_bin(&self) -> Result<(), BinstallError> {
        self.check_source_exists()?;

        debug!(
            "Atomically install file from '{}' to '{}'",
            self.source.display(),
            self.dest.display()
        );
        atomic_install(&self.source, &self.dest)?;

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), BinstallError> {
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
        atomic_symlink_file(dest, &self.link)?;

        Ok(())
    }

    fn link_dest(&self) -> &Path {
        if cfg!(target_family = "unix") {
            Path::new(self.dest.file_name().unwrap())
        } else {
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

impl<'c> Context<'c> {
    fn render(&self, template: &str) -> Result<String, BinstallError> {
        let mut tt = TinyTemplate::new();
        tt.add_template("path", template)?;
        Ok(tt.render("path", self)?)
    }
}
