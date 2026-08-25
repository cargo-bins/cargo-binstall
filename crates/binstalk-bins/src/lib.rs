use std::{
    borrow::Cow,
    fmt, fs, io,
    path::{self, Component, Path, PathBuf},
};

use atomic_file_install::{
    atomic_install, atomic_install_noclobber, atomic_symlink_file, atomic_symlink_file_noclobber,
};
use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta};
use compact_str::{CompactString, format_compact};
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

/// Directories in the extracted archive that may hold the binaries of
/// `name`, in the order [`infer_bin_dir_template`] tries them.
///
/// SUPPORT.md lists these, see binstalk-fetchers' `support_md` test.
///
/// Make sure to update
/// fetchers::gh_crate_meta::hosting::{FULL_FILENAMES,
/// NOVERSION_FILENAMES} if you update this list.
pub fn possible_bin_dirs<'a>(
    name: &'a str,
    target: &'a str,
    version: &'a str,
) -> impl Iterator<Item = String> + 'a {
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

    gen_possible_dirs
        .into_iter()
        .map(move |gen_possible_dir| gen_possible_dir(name, target, version))
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

    let default_bin_dir_template = Cow::Borrowed("{ bin }{ binary-ext }");

    possible_bin_dirs(name, target, version)
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

/// An extra name symlinked to the installed binary.
pub struct AliasLink {
    /// Alias file name, including the binary extension (e.g. `.exe` on Windows).
    pub name: CompactString,
    /// Absolute path of the alias symlink, i.e. `install_path/<name>`.
    pub path: PathBuf,
}

pub struct BinFile {
    pub base_name: CompactString,
    pub source: PathBuf,
    pub archive_source_path: PathBuf,
    pub dest: PathBuf,
    pub link: Option<PathBuf>,
    /// The dir `source` must stay inside of.
    extracted_dir: PathBuf,
    /// Extra symlinks pointing at the same target as `link`.
    ///
    /// Always empty when `link` is `None`, since aliases are symlinks too.
    pub aliases: Vec<AliasLink>,
}

impl BinFile {
    /// * `tt` - must have a template with name "bin_dir"
    pub fn new(
        data: &Data<'_>,
        base_name: &str,
        tt: &Template<'_>,
        no_symlinks: bool,
        aliases: &[CompactString],
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

        let (source, archive_source_path, extracted_dir) = if data.meta.pkg_fmt == Some(PkgFmt::Bin)
        {
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

        // Aliases are symlinks, so they only make sense when the primary
        // symlink is created too.
        let aliases = if link.is_some() {
            aliases
                .iter()
                .map(|alias| {
                    let name = format_compact!("{alias}{binary_ext}");
                    let path = data.install_path.join(name.as_str());
                    AliasLink { name, path }
                })
                .collect()
        } else {
            Vec::new()
        };

        Ok(Self {
            base_name: format_compact!("{base_name}{binary_ext}"),
            source,
            archive_source_path,
            dest,
            link,
            extracted_dir,
            aliases,
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
            aliases: &self.aliases,
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

    /// Return the source with symlinks resolved.
    fn pre_install_bin(&self) -> Result<PathBuf, Error> {
        let source = match fs::canonicalize(&self.source) {
            Ok(source) => source,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(Error::BinFileNotFound((&*self.source).into()));
            }
            Err(err) => return Err(err.into()),
        };

        // The archive may contain symlinks, as the bin itself or as a dir on its path, pointing
        // outside of the extracted dir. Following them would chmod and install a file that does
        // not come from the archive.
        if !source.starts_with(fs::canonicalize(&self.extracted_dir)?) {
            return Err(Error::InvalidSourceFilePath((&*self.source).into()));
        }

        #[cfg(unix)]
        fs::set_permissions(&source, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;

        Ok(source)
    }

    pub fn install_bin(&self) -> Result<(), Error> {
        let source = self.pre_install_bin()?;

        debug!(
            "Atomically install file from '{}' to '{}'",
            self.source.display(),
            self.dest.display()
        );

        atomic_install(&source, &self.dest)?;

        Ok(())
    }

    pub fn install_bin_noclobber(&self) -> Result<(), Error> {
        let source = self.pre_install_bin()?;

        debug!(
            "Installing file from '{}' to '{}' only if dst not exists",
            self.source.display(),
            self.dest.display()
        );

        atomic_install_noclobber(&source, &self.dest)?;

        Ok(())
    }

    pub fn install_link(&self) -> Result<(), Error> {
        if let Some(link) = &self.link {
            let dest = self.link_dest();
            debug!(
                "Create link '{}' pointing to '{}'",
                link.display(),
                dest.display()
            );
            atomic_symlink_file(dest, link)?;

            for alias in &self.aliases {
                debug!(
                    "Create alias link '{}' pointing to '{}'",
                    alias.path.display(),
                    dest.display()
                );
                atomic_symlink_file(dest, &alias.path)?;
            }
        }

        Ok(())
    }

    pub fn install_link_noclobber(&self) -> Result<(), Error> {
        if let Some(link) = &self.link {
            let dest = self.link_dest();
            debug!(
                "Create link '{}' pointing to '{}' only if dst not exists",
                link.display(),
                dest.display()
            );
            atomic_symlink_file_noclobber(dest, link)?;

            for alias in &self.aliases {
                debug!(
                    "Create alias link '{}' pointing to '{}' only if dst not exists",
                    alias.path.display(),
                    dest.display()
                );
                atomic_symlink_file_noclobber(dest, &alias.path)?;
            }
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
    aliases: &'a [AliasLink],
}

impl fmt::Display for LazyFormat<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} -> {})", self.base_name, self.source, self.dest)?;
        if !self.aliases.is_empty() {
            write!(f, ", aliases: ")?;
            for (i, alias) in self.aliases.iter().enumerate() {
                if i != 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", alias.name)?;
            }
        }
        Ok(())
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
    use super::*;

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

    #[cfg(unix)]
    mod symlink {
        use super::*;
        use std::os::unix::fs::{PermissionsExt, symlink};

        fn bin_file(extracted: &Path, archive_source_path: &str, install: &Path) -> BinFile {
            BinFile {
                base_name: "bin".into(),
                source: extracted.join(archive_source_path),
                archive_source_path: archive_source_path.into(),
                dest: install.join("bin"),
                link: None,
                extracted_dir: extracted.to_path_buf(),
                aliases: Vec::new(),
            }
        }

        /// A tempdir with an `extracted` and an `install` dir, and a file
        /// `outside` of the extracted dir.
        fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
            let tmp = tempfile::tempdir().unwrap();
            let extracted = tmp.path().join("extracted");
            let install = tmp.path().join("install");
            fs::create_dir(&extracted).unwrap();
            fs::create_dir(&install).unwrap();

            let outside = tmp.path().join("outside");
            fs::write(&outside, b"outside").unwrap();
            fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();

            (tmp, extracted, install, outside)
        }

        fn assert_untouched(outside: &Path, install: &Path) {
            let mode = fs::metadata(outside).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            assert_eq!(fs::read(outside).unwrap(), b"outside");
            assert!(fs::symlink_metadata(install.join("bin")).is_err());
        }

        #[test]
        fn bin_symlink_outside() {
            let (_tmp, extracted, install, outside) = setup();
            symlink(&outside, extracted.join("bin")).unwrap();

            let bin_file = bin_file(&extracted, "bin", &install);
            for res in [bin_file.install_bin(), bin_file.install_bin_noclobber()] {
                assert!(
                    matches!(res, Err(Error::InvalidSourceFilePath(_))),
                    "{res:?}"
                );
            }
            assert_untouched(&outside, &install);
        }

        #[test]
        fn dir_symlink_outside() {
            let (tmp, extracted, install, outside) = setup();
            symlink(tmp.path(), extracted.join("dir")).unwrap();

            let bin_file = bin_file(&extracted, "dir/outside", &install);
            let res = bin_file.install_bin();
            assert!(
                matches!(res, Err(Error::InvalidSourceFilePath(_))),
                "{res:?}"
            );
            assert_untouched(&outside, &install);
        }

        #[test]
        fn bin_symlink_dangling() {
            let (_tmp, extracted, install, _outside) = setup();
            symlink("missing", extracted.join("bin")).unwrap();

            let res = bin_file(&extracted, "bin", &install).install_bin();
            assert!(matches!(res, Err(Error::BinFileNotFound(_))), "{res:?}");
        }

        /// A symlink inside of the extracted dir is fine, and its target is
        /// installed instead of the symlink itself.
        #[test]
        fn bin_symlink_inside() {
            let (_tmp, extracted, install, _outside) = setup();
            fs::write(extracted.join("real-bin"), b"bin").unwrap();
            symlink("real-bin", extracted.join("bin")).unwrap();

            bin_file(&extracted, "bin", &install).install_bin().unwrap();

            let installed = install.join("bin");
            assert!(fs::symlink_metadata(&installed).unwrap().is_file());
            assert_eq!(fs::read(&installed).unwrap(), b"bin");
            let mode = fs::metadata(&installed).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    mod aliases {
        use super::*;
        use binstalk_types::cargo_toml_binstall::PkgMeta;

        struct NoValues;
        impl leon::Values for NoValues {
            fn get_value(&self, _key: &str) -> Option<Cow<'_, str>> {
                None
            }
        }

        fn bin_file(target: &str, no_symlinks: bool, aliases: &[CompactString]) -> BinFile {
            let bin_path = Path::new("/tmp/pkg/mycrate");
            let install_path = Path::new("/home/user/.cargo/bin");
            let data = Data {
                name: "mycrate",
                target,
                version: "1.2.3",
                repo: None,
                meta: PkgMeta {
                    pkg_fmt: Some(PkgFmt::Bin),
                    ..Default::default()
                },
                bin_path,
                install_path,
                target_related_info: &NoValues,
            };

            // With `PkgFmt::Bin` the template is never rendered, but `new` still
            // requires one.
            let tt = Template::parse("{ bin }{ binary-ext }").unwrap();
            BinFile::new(&data, "mycrate", &tt, no_symlinks, aliases).unwrap()
        }

        #[test]
        fn aliases_symlink_alongside_the_binary() {
            let bin = bin_file(
                "x86_64-unknown-linux-gnu",
                false,
                &["mcr".into(), "myc".into()],
            );

            assert_eq!(
                bin.link.as_deref(),
                Some(Path::new("/home/user/.cargo/bin/mycrate"))
            );
            assert_eq!(bin.dest, Path::new("/home/user/.cargo/bin/mycrate-v1.2.3"));

            let aliases: Vec<_> = bin
                .aliases
                .iter()
                .map(|alias| (alias.name.as_str(), alias.path.as_path()))
                .collect();
            assert_eq!(
                aliases,
                [
                    ("mcr", Path::new("/home/user/.cargo/bin/mcr")),
                    ("myc", Path::new("/home/user/.cargo/bin/myc")),
                ]
            );
        }

        #[test]
        fn aliases_get_the_windows_binary_extension() {
            let bin = bin_file("x86_64-pc-windows-msvc", false, &["mcr".into()]);

            assert_eq!(bin.aliases.len(), 1);
            assert_eq!(bin.aliases[0].name, "mcr.exe");
            assert_eq!(
                bin.aliases[0].path,
                Path::new("/home/user/.cargo/bin/mcr.exe")
            );
        }

        #[test]
        fn aliases_are_dropped_without_symlinks() {
            let bin = bin_file("x86_64-unknown-linux-gnu", true, &["mcr".into()]);

            assert!(bin.link.is_none());
            assert!(bin.aliases.is_empty());
        }
    }
}
