use std::{
    borrow::Cow,
    fmt, io,
    path::{self, Component, Path, PathBuf},
};

use atomic_file_install::{
    atomic_install_in, atomic_install_noclobber_in, atomic_symlink_file_in,
    atomic_symlink_file_noclobber_in,
};
use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta};
use cap_std::{ambient_authority, fs::Dir};
use compact_str::{format_compact, CompactString};
use leon::Template;
use miette::Diagnostic;
use normalize_path::NormalizePath;
use thiserror::Error as ThisError;
use tracing::debug;

#[derive(Debug, ThisError, Diagnostic)]
pub enum Error {
    /// bin-dir configuration provided generates source path outside
    /// of the temporary dir.
    #[error(
        "bin-dir configuration provided generates source path outside of the temporary dir: {}", .0.display()
    )]
    InvalidSourceFilePath(Box<Path>),

    /// bin-dir configuration provided generates empty source path.
    #[error("bin-dir configuration provided generates empty source path")]
    EmptySourceFilePath,

    /// Bin file is not found.
    #[error("bin file {} not found", .0.display())]
    BinFileNotFound(Box<Path>),

    /// Bin name from the crate manifest is not a plain file name, so it
    /// would place the binary outside of the install dir.
    #[error("invalid bin name {0:?}: must be a plain file name")]
    InvalidBinName(CompactString),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("Failed to render template: {0}")]
    #[diagnostic(transparent)]
    TemplateRender(#[from] leon::RenderError),
}

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

/// Return true if `name` is safe to use as a file name inside the install dir.
///
/// Bin names come from the crate's `Cargo.toml`, which is controlled by the
/// crate publisher, so they must not contain anything that lets
/// `install_path.join(name)` escape `install_path`: path separators of any
/// platform, `.`/`..`, drive prefixes (`C:`) or NUL.
///
/// The check is the same on every platform so that a name rejected on
/// Windows is also rejected on unix and vice versa.
pub fn is_valid_bin_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && matches!(
            Path::new(name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
}

/// Must be called after the archive is downloaded and extracted.
/// This function might uses blocking I/O.
pub fn infer_bin_dir_template(
    data: &Data,
    has_dir: &mut dyn FnMut(&Path) -> bool,
) -> Cow<'static, str> {
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
        .find(|dirname| has_dir(Path::new(&dirname)))
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
    pub archive_source_path: PathBuf,
    pub dest: PathBuf,
    pub link: Option<PathBuf>,
    /// The directory `archive_source_path` is relative to.
    source_root: PathBuf,
    /// The directory `dest` and `link` are in.
    install_path: PathBuf,
}

impl BinFile {
    /// * `tt` - must have a template with name "bin_dir"
    pub fn new(
        data: &Data<'_>,
        base_name: &str,
        tt: &Template<'_>,
        no_symlinks: bool,
    ) -> Result<Self, Error> {
        if !is_valid_bin_name(base_name) {
            return Err(Error::InvalidBinName(base_name.into()));
        }

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
            binary_ext,

            target_related_info: data.target_related_info,
        };

        let (source, archive_source_path, source_root) = if data.meta.pkg_fmt == Some(PkgFmt::Bin) {
            (
                data.bin_path.to_path_buf(),
                data.bin_path.file_name().unwrap().into(),
                data.bin_path.parent().unwrap().to_path_buf(),
            )
        } else {
            // Generate install paths
            // Source path is the download dir + the generated binary path
            let path = tt.render(&ctx)?;

            let path_normalized = Path::new(&path).normalize();

            if path_normalized.components().next().is_none() {
                return Err(Error::EmptySourceFilePath);
            }

            if !is_valid_path(&path_normalized) {
                return Err(Error::InvalidSourceFilePath(path_normalized.into()));
            }

            (
                data.bin_path.join(&path_normalized),
                path_normalized,
                data.bin_path.to_path_buf(),
            )
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
            archive_source_path,
            dest,
            link,
            source_root,
            install_path: data.install_path.to_path_buf(),
        })
    }

    pub fn preview_bin(&self) -> impl fmt::Display + '_ {
        struct PreviewBin<'a> {
            base_name: &'a str,
            dest: path::Display<'a>,
        }

        impl fmt::Display for PreviewBin<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} => {}", self.base_name, self.dest)
            }
        }

        PreviewBin {
            base_name: &self.base_name,
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
        has_file: &mut dyn FnMut(&Path) -> bool,
    ) -> Result<(), Error> {
        if has_file(&self.archive_source_path) {
            Ok(())
        } else {
            Err(Error::BinFileNotFound((&*self.source).into()))
        }
    }

    /// Open the source and install dirs, and prepare the source bin.
    fn pre_install_bin(&self) -> Result<(Dir, Dir), Error> {
        let source_dir = open_dir(&self.source_root)?;

        if !source_dir.try_exists(&self.archive_source_path)? {
            return Err(Error::BinFileNotFound((&*self.source).into()));
        }

        #[cfg(unix)]
        source_dir.set_permissions(
            &self.archive_source_path,
            cap_std::fs::Permissions::from_std(std::os::unix::fs::PermissionsExt::from_mode(0o755)),
        )?;

        Ok((source_dir, open_dir(&self.install_path)?))
    }

    pub fn install_bin(&self) -> Result<(), Error> {
        let (source_dir, install_dir) = self.pre_install_bin()?;

        debug!(
            "Atomically install file from '{}' to '{}'",
            self.source.display(),
            self.dest.display()
        );

        atomic_install_in(
            (&source_dir, &self.archive_source_path),
            (&install_dir, file_name(&self.dest)),
        )?;

        Ok(())
    }

    pub fn install_bin_noclobber(&self) -> Result<(), Error> {
        let (source_dir, install_dir) = self.pre_install_bin()?;

        debug!(
            "Installing file from '{}' to '{}' only if dst not exists",
            self.source.display(),
            self.dest.display()
        );

        atomic_install_noclobber_in(
            (&source_dir, &self.archive_source_path),
            (&install_dir, file_name(&self.dest)),
        )?;

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), Error> {
        if let Some(link) = &self.link {
            debug!(
                "Create link '{}' pointing to '{}'",
                link.display(),
                self.link_dest().display()
            );
            atomic_symlink_file_in(
                file_name(&self.dest),
                (&open_dir(&self.install_path)?, file_name(link)),
            )?;
        }

        Ok(())
    }

    pub fn install_link_noclobber(&self) -> Result<(), Error> {
        if let Some(link) = &self.link {
            debug!(
                "Create link '{}' pointing to '{}' only if dst not exists",
                link.display(),
                self.link_dest().display()
            );
            atomic_symlink_file_noclobber_in(
                file_name(&self.dest),
                (&open_dir(&self.install_path)?, file_name(link)),
            )?;
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

fn open_dir(path: &Path) -> io::Result<Dir> {
    Dir::open_ambient_dir(path, ambient_authority())
}

/// `dest` and `link` always have a file name, since they are the install
/// dir joined with a valid bin name.
fn file_name(path: &Path) -> &Path {
    Path::new(path.file_name().unwrap())
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
    /// More target related info, it's recommend to provide the following keys:
    ///  - target_family,
    ///  - target_arch
    ///  - target_libc
    ///  - target_vendor
    pub target_related_info: &'a dyn leon::Values,
}

#[derive(Clone)]
struct Context<'c> {
    name: &'c str,
    repo: Option<&'c str>,
    target: &'c str,
    version: &'c str,
    bin: &'c str,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    binary_ext: &'c str,

    target_related_info: &'c dyn leon::Values,
}

impl leon::Values for Context<'_> {
    fn get_value<'s>(&'s self, key: &str) -> Option<Cow<'s, str>> {
        match key {
            "name" => Some(Cow::Borrowed(self.name)),
            "repo" => self.repo.map(Cow::Borrowed),
            "target" => Some(Cow::Borrowed(self.target)),
            "version" => Some(Cow::Borrowed(self.version)),
            "bin" => Some(Cow::Borrowed(self.bin)),
            "binary-ext" => Some(Cow::Borrowed(self.binary_ext)),
            // Soft-deprecated alias for binary-ext
            "format" => Some(Cow::Borrowed(self.binary_ext)),

            key => self.target_related_info.get_value(key),
        }
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

#[cfg(test)]
mod tests {
    use super::is_valid_bin_name;

    #[test]
    fn valid_bin_names() {
        for name in [
            "foo",
            "foo-bar",
            "foo_bar",
            "foo.bar",
            "cargo-binstall",
            "..foo",
            "foo..",
        ] {
            assert!(is_valid_bin_name(name), "{name:?} should be valid");
        }
    }

    #[test]
    fn invalid_bin_names() {
        for name in [
            "",
            ".",
            "..",
            "../foo",
            "../../.bashrc",
            "foo/bar",
            "foo/",
            "/etc/passwd",
            "..\\foo",
            "foo\\bar",
            "C:foo",
            "C:\\Windows\\foo",
            "\\\\server\\share\\foo",
            "foo\0",
        ] {
            assert!(!is_valid_bin_name(name), "{name:?} should be invalid");
        }
    }
}
