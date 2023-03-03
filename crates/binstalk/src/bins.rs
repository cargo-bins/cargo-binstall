use std::{
    borrow::Cow,
    fmt,
    path::{self, Component, Path, PathBuf},
};

use compact_str::{format_compact, CompactString};
use normalize_path::NormalizePath;
use serde::Serialize;
use tinytemplate::TinyTemplate;
use tracing::debug;

use crate::{
    errors::BinstallError,
    fs::{atomic_install, atomic_symlink_file},
    helpers::download::ExtractedFiles,
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

/// Return true if the path does not look outside of current dir
///
///  * `path` - must be normalized before passing to this function
fn is_valid_path(path: &Path) -> bool {
    !matches!(
        path.components().next(),
        // normalized path cannot have curdir or parentdir,
        // so checking prefix/rootdir is enough.
        Some(Component::Prefix(..) | Component::RootDir)
    )
}

/// Must be called after the archive is downloaded and extracted.
/// This function might uses blocking I/O.
pub fn infer_bin_dir_template(data: &Data, extracted_files: &ExtractedFiles) -> Cow<'static, str> {
    let name = data.name;
    let target = data.target;
    let version = data.version;

    // Make sure to update
    // fetchers::gh_crate_meta::hosting::{FULL_FILENAMES,
    // NOVERSION_FILENAMES} if you update this array.
    let gen_possible_dirs: [for<'r> fn(&'r str, &'r str, &'r str) -> String; 8] = [
        |name, target, version| format!("{name}-{target}-v{version}"),
        |name, target, version| format!("{name}-{target}-{version}"),
        |name, target, version| format!("{name}-{version}-{target}"),
        |name, target, version| format!("{name}-v{version}-{target}"),
        |name, target, _version| format!("{name}-{target}"),
        // Ignore the following when updating hosting::{FULL_FILENAMES, NOVERSION_FILENAMES}
        |name, _target, version| format!("{name}-{version}"),
        |name, _target, version| format!("{name}-v{version}"),
        |name, _target, _version| name.to_string(),
    ];

    let default_bin_dir_template = Cow::Borrowed("{ bin }{ binary-ext }");

    gen_possible_dirs
        .into_iter()
        .map(|gen_possible_dir| gen_possible_dir(name, target, version))
        .find(|dirname| {
            extracted_files
                .get_dir(&data.bin_path.join(dirname))
                .is_some()
        })
        .map(|mut dir| {
            dir.reserve_exact(1 + default_bin_dir_template.len());
            dir += "/";
            dir += &default_bin_dir_template;
            Cow::Owned(dir)
        })
        // Fallback to no dir
        .unwrap_or(default_bin_dir_template)
}

pub struct BinFile {
    pub base_name: CompactString,
    pub source: PathBuf,
    pub dest: PathBuf,
    pub link: Option<PathBuf>,
}

impl BinFile {
    /// * `tt` - must have a template with name "bin_dir"
    pub fn new(
        data: &Data<'_>,
        base_name: &str,
        tt: &TinyTemplate,
        no_symlinks: bool,
    ) -> Result<Self, BinstallError> {
        let binary_ext = if data.target.contains("windows") {
            ".exe"
        } else {
            ""
        };

        let ctx = Context {
            name: data.name,
            repo: data.repo,
            target: data.target,
            version: data.version,
            bin: base_name,
            format: binary_ext,
            binary_ext,
        };

        let source = if data.meta.pkg_fmt == Some(PkgFmt::Bin) {
            data.bin_path.to_path_buf()
        } else {
            // Generate install paths
            // Source path is the download dir + the generated binary path
            let path = ctx.render_with_compiled_tt(tt)?;

            let path_normalized = Path::new(&path).normalize();

            if path_normalized.components().next().is_none() {
                return Err(BinstallError::EmptySourceFilePath);
            }

            if !is_valid_path(&path_normalized) {
                return Err(BinstallError::InvalidSourceFilePath {
                    path: path_normalized,
                });
            }

            data.bin_path.join(&path_normalized)
        };

        // Destination at install dir + base-name{.extension}
        let mut dest = data.install_path.join(ctx.bin);
        if !binary_ext.is_empty() {
            let binary_ext = binary_ext.strip_prefix('.').unwrap();

            // PathBuf::set_extension returns false if Path::file_name
            // is None, but we know that the file name must be Some,
            // thus we assert! the return value here.
            assert!(dest.set_extension(binary_ext));
        }

        let (dest, link) = if no_symlinks {
            (dest, None)
        } else {
            // Destination path is the install dir + base-name-version{.extension}
            let dest_file_path_with_ver = format!("{}-v{}{}", ctx.bin, ctx.version, ctx.binary_ext);
            let dest_with_ver = data.install_path.join(dest_file_path_with_ver);

            (dest_with_ver, Some(dest))
        };

        Ok(Self {
            base_name: format_compact!("{base_name}{binary_ext}"),
            source,
            dest,
            link,
        })
    }

    pub fn preview_bin(&self) -> impl fmt::Display + '_ {
        LazyFormat {
            base_name: &self.base_name,
            source: Path::new(self.source.file_name().unwrap()).display(),
            dest: self.dest.display(),
        }
    }

    pub fn preview_link(&self) -> impl fmt::Display + '_ {
        OptionalLazyFormat(self.link.as_ref().map(|link| LazyFormat {
            base_name: &self.base_name,
            source: link.display(),
            dest: self.link_dest().display(),
        }))
    }

    /// Return `Ok` if the source exists, otherwise `Err`.
    pub fn check_source_exists(
        &self,
        extracted_files: &ExtractedFiles,
    ) -> Result<(), BinstallError> {
        if extracted_files.has_file(&self.source) {
            Ok(())
        } else {
            Err(BinstallError::BinFileNotFound(self.source.clone()))
        }
    }

    pub fn install_bin(&self) -> Result<(), BinstallError> {
        if !self.source.try_exists()? {
            return Err(BinstallError::BinFileNotFound(self.source.clone()));
        }

        debug!(
            "Atomically install file from '{}' to '{}'",
            self.source.display(),
            self.dest.display()
        );

        #[cfg(unix)]
        std::fs::set_permissions(
            &self.source,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )?;

        atomic_install(&self.source, &self.dest)?;

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), BinstallError> {
        if let Some(link) = &self.link {
            let dest = self.link_dest();
            debug!(
                "Create link '{}' pointing to '{}'",
                link.display(),
                dest.display()
            );
            atomic_symlink_file(dest, link)?;
        }

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
pub struct Data<'a> {
    pub name: &'a str,
    pub target: &'a str,
    pub version: &'a str,
    pub repo: Option<&'a str>,
    pub meta: PkgMeta,
    pub bin_path: &'a Path,
    pub install_path: &'a Path,
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
    /// * `tt` - must have a template with name "bin_dir"
    fn render_with_compiled_tt(&self, tt: &TinyTemplate) -> Result<String, BinstallError> {
        Ok(tt.render("bin_dir", self)?)
    }
}

struct LazyFormat<'a> {
    base_name: &'a str,
    source: path::Display<'a>,
    dest: path::Display<'a>,
}

impl fmt::Display for LazyFormat<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} -> {})", self.base_name, self.source, self.dest)
    }
}

struct OptionalLazyFormat<'a>(Option<LazyFormat<'a>>);

impl fmt::Display for OptionalLazyFormat<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(lazy_format) = self.0.as_ref() {
            fmt::Display::fmt(lazy_format, f)
        } else {
            Ok(())
        }
    }
}
