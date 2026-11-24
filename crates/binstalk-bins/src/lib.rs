use std::{
    borrow::Cow,
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fmt, fs, io, iter,
    path::{self, Component, Path, PathBuf},
};

use atomic_file_install::{
    atomic_install, atomic_install_noclobber, atomic_symlink_file, atomic_symlink_file_noclobber,
};
use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta};
use compact_str::{CompactString, format_compact};
use itertools::Itertools;
use leon::Template;
use miette::Diagnostic;
use normalize_path::NormalizePath;
use thiserror::Error as ThisError;
use tracing::debug;

type SymlinkFp = fn(&Path, &Path) -> io::Result<()>;

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

    /// `binary.NAME` is set for a binary the crate does not have.
    #[error("`binary.{0}` is set, but the crate has no binary named {0}")]
    UnknownBinary(CompactString),

    /// Bin file is not found.
    #[error("bin file {} not found", .0.display())]
    BinFileNotFound(Box<Path>),

    /// Bin name from the crate manifest is not a plain file name, so it
    /// would place the binary outside of the install dir.
    #[error("invalid bin name {0:?}: must be a plain file name")]
    InvalidBinName(CompactString),

    /// An alias has the name of a binary, or of another alias, of the crate,
    /// so installing it would replace that file.
    #[error("alias {0:?} clashes with a binary or another alias of the same name")]
    AliasClash(CompactString),

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

/// Return `Err` if an alias of any of `bin_files` has the same file name as
/// a file installed for one of the binaries (`dest` or `link`), or as
/// another alias.
pub fn check_aliases_unique(bin_files: &[BinFile]) -> Result<(), Error> {
    let mut names: BTreeSet<&OsStr> = bin_files
        .iter()
        .flat_map(|bin| iter::once(&bin.dest).chain(&bin.link))
        .filter_map(|path| path.file_name())
        .collect();

    bin_files
        .iter()
        .flat_map(|bin| &bin.aliases)
        .try_for_each(|alias| {
            let name = alias.file_name().unwrap();
            if names.insert(name) {
                Ok(())
            } else {
                Err(Error::AliasClash(name.to_string_lossy().into()))
            }
        })
}

/// Path of the file `name` with `binary_ext` appended, in `install_path`.
///
/// The extension is appended rather than set with [`PathBuf::set_extension`],
/// which would replace the part of `name` after a dot.
fn install_file_path(install_path: &Path, name: &str, binary_ext: &str) -> PathBuf {
    install_path.join(bin_file_name(name, binary_ext))
}

/// `name` with `binary_ext` appended.
fn bin_file_name(name: &str, binary_ext: &str) -> OsString {
    let mut file_name = OsString::with_capacity(name.len() + binary_ext.len());
    file_name.push(name);
    file_name.push(binary_ext);
    file_name
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
///
/// * `sole_root_dir` - the name of the only entry at the top of the
///   extracted archive, if it is a dir. It is tried after
///   [`possible_bin_dirs`], as archives named in other ways, such as
///   `foo-1.2.3-x86_64-linux.tar.xz`, usually hold a dir named like them.
pub fn infer_bin_dir_template(
    data: &Data,
    has_dir: &mut dyn FnMut(&Path) -> bool,
    sole_root_dir: Option<&str>,
) -> Cow<'static, str> {
    let name = data.name;
    let target = data.target;
    let version = data.version;

    let default_bin_dir_template = Cow::Borrowed("{ bin }{ binary-ext }");

    possible_bin_dirs(name, target, version)
        .find(|dirname| has_dir(Path::new(&dirname)))
        .or_else(|| {
            // It is a file name in the archive, not a template.
            let mut dir = String::new();
            for c in sole_root_dir?.chars() {
                if matches!(c, '\\' | '{' | '}') {
                    dir.push('\\');
                }
                dir.push(c);
            }
            Some(dir)
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
    pub archive_source_path: PathBuf,
    pub dest: PathBuf,
    pub link: Option<PathBuf>,
    /// The dir `source` must stay inside of.
    extracted_dir: PathBuf,
    /// Extra symlinks in the install dir pointing at `dest`, like `link`,
    /// with the binary extension (e.g. `.exe` on Windows).
    ///
    /// They are created whether or not `link` is.
    pub aliases: Vec<PathBuf>,
}

impl BinFile {
    /// * `tt` - the `bin-dir` template, rendering to the path of the binary
    ///   in the extracted archive, or to the dir holding it.
    /// * `has_dir` - returns true if the path is a dir in the extracted
    ///   archive. If the rendered `tt` is one, the binary is looked up in
    ///   it as `{ bin }{ binary-ext }`.
    pub fn new(
        data: &Data<'_>,
        base_name: &str,
        tt: &Template<'_>,
        has_dir: &mut dyn FnMut(&Path) -> bool,
        no_symlinks: bool,
        aliases: &[CompactString],
    ) -> Result<Self, Error> {
        if !is_valid_bin_name(base_name) {
            return Err(Error::InvalidBinName(base_name.into()));
        }
        // An alias becomes a file name in the install dir, just like a bin name.
        if let Some(alias) = aliases.iter().find(|alias| !is_valid_bin_name(alias)) {
            return Err(Error::InvalidBinName(alias.clone()));
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

        // The download is the binary itself, raw or compressed on its own.
        let is_single_file = data.meta.pkg_fmt.is_some_and(PkgFmt::is_single_file);

        let (source, archive_source_path, extracted_dir) = if is_single_file {
            (
                data.bin_path.to_path_buf(),
                data.bin_path.file_name().unwrap().into(),
                data.bin_path.parent().unwrap().to_path_buf(),
            )
        } else {
            // Generate install paths
            // Source path is the download dir + the generated binary path
            let path = tt.render(&ctx)?;

            let mut path_normalized = Path::new(&path).normalize();

            if path_normalized.components().next().is_none() {
                return Err(Error::EmptySourceFilePath);
            }

            if !is_valid_path(&path_normalized) {
                return Err(Error::InvalidSourceFilePath(path_normalized.into()));
            }

            // `bin-dir` may name the dir holding the binary instead of the
            // binary itself. `base_name` is a plain file name, so this
            // cannot leave the dir.
            if has_dir(&path_normalized) {
                path_normalized.push(bin_file_name(base_name, binary_ext));
            }

            (
                data.bin_path.join(&path_normalized),
                path_normalized,
                data.bin_path.to_path_buf(),
            )
        };

        // Destination at install dir + base-name{.extension}
        let dest = install_file_path(data.install_path, ctx.bin, binary_ext);

        let (dest, link) = if no_symlinks {
            (dest, None)
        } else {
            // Destination path is the install dir + base-name-version{.extension}
            let dest_file_path_with_ver = format!("{}-v{}{}", ctx.bin, ctx.version, ctx.binary_ext);
            let dest_with_ver = data.install_path.join(dest_file_path_with_ver);

            (dest_with_ver, Some(dest))
        };

        let aliases = aliases
            .iter()
            .map(|alias| install_file_path(data.install_path, alias, binary_ext))
            .collect();

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
            aliases: &'a [PathBuf],
        }

        impl fmt::Display for PreviewBin<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} => {}", self.base_name, self.dest)?;
                if !self.aliases.is_empty() {
                    let names = self.aliases.iter().map(|alias| {
                        // Aliases are always created with a file name.
                        alias.file_name().unwrap().display()
                    });
                    write!(f, " (aliases: {})", names.format(", "))?;
                }
                Ok(())
            }
        }

        PreviewBin {
            base_name: &self.base_name,
            dest: self.dest.display(),
            aliases: &self.aliases,
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

    /// Create `link` (if any) and the aliases, pointing at `dest`.
    ///
    /// On Windows, a copy of `dest` is made if creating a symlink fails.
    pub fn install_link(&self) -> Result<(), Error> {
        self.install_link_with(atomic_symlink_file, "")
    }

    pub fn install_link_noclobber(&self) -> Result<(), Error> {
        self.install_link_with(atomic_symlink_file_noclobber, " only if dst not exists")
    }

    fn install_link_with(&self, symlink: SymlinkFp, extra_msg: &str) -> Result<(), Error> {
        let dest = self.link_dest();

        for link in self.link.iter().chain(&self.aliases) {
            debug!(
                "Create link '{}' pointing to '{}'{extra_msg}",
                link.display(),
                dest.display()
            );
            symlink(dest, link)?;
        }

        Ok(())
    }

    /// Target of the symlinks to `dest` created in the install dir.
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
    use super::*;

    #[test]
    fn infer_bin_dir() {
        let data = Data {
            name: "foo",
            target: "x86_64-unknown-linux-gnu",
            version: "1.2.3",
            repo: None,
            meta: PkgMeta::default(),
            bin_path: Path::new("/tmp/pkg"),
            install_path: Path::new("/tmp/bin"),
            target_related_info: &leon::vals(|_| None),
        };
        let infer = |dirs: &[&str], sole_root_dir| {
            infer_bin_dir_template(
                &data,
                &mut |path| dirs.iter().any(|dir| Path::new(dir) == path),
                sole_root_dir,
            )
        };

        assert_eq!(infer(&[], None), "{ bin }{ binary-ext }");
        assert_eq!(
            infer(&["foo-x86_64-unknown-linux-gnu"], None),
            "foo-x86_64-unknown-linux-gnu/{ bin }{ binary-ext }"
        );
        // The dirs named after the crate come first.
        assert_eq!(
            infer(&["foo-v1.2.3"], Some("foo-1.2.3-x86_64-linux")),
            "foo-v1.2.3/{ bin }{ binary-ext }"
        );
        assert_eq!(
            infer(&[], Some("foo-1.2.3-x86_64-linux")),
            "foo-1.2.3-x86_64-linux/{ bin }{ binary-ext }"
        );

        // It is not a template.
        let template = infer(&[], Some(r"{ name }\"));
        assert_eq!(template, r"\{ name \}\\/{ bin }{ binary-ext }");
        assert_eq!(
            Template::parse(&template)
                .unwrap()
                .render(&[("bin", "foo"), ("binary-ext", "")])
                .unwrap(),
            r"{ name }\/foo"
        );
    }

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

        fn new_bin_file(
            bin_path: &Path,
            install_path: &Path,
            target: &str,
            no_symlinks: bool,
            aliases: &[&str],
        ) -> Result<BinFile, Error> {
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
            let aliases: Vec<CompactString> = aliases.iter().copied().map(Into::into).collect();

            // With `PkgFmt::Bin` the template is never rendered, but `new` still
            // requires one.
            let tt = Template::parse("{ bin }{ binary-ext }").unwrap();
            BinFile::new(&data, "mycrate", &tt, &mut |_| false, no_symlinks, &aliases)
        }

        fn bin_file(target: &str, no_symlinks: bool, aliases: &[&str]) -> BinFile {
            new_bin_file(
                Path::new("/tmp/pkg/mycrate"),
                Path::new("/home/user/.cargo/bin"),
                target,
                no_symlinks,
                aliases,
            )
            .unwrap()
        }

        #[test]
        fn aliases_are_named_alongside_the_binary() {
            let bin = bin_file("x86_64-unknown-linux-gnu", true, &["mcr", "myc"]);

            assert!(bin.link.is_none());
            assert_eq!(bin.dest, Path::new("/home/user/.cargo/bin/mycrate"));
            assert_eq!(
                bin.aliases,
                [
                    Path::new("/home/user/.cargo/bin/mcr"),
                    Path::new("/home/user/.cargo/bin/myc"),
                ]
            );
            assert_eq!(
                bin.preview_bin().to_string(),
                "mycrate => /home/user/.cargo/bin/mycrate (aliases: mcr, myc)"
            );
        }

        #[test]
        fn single_file_formats_install_the_download_itself() {
            let source = |pkg_fmt| {
                let data = Data {
                    name: "mycrate",
                    target: "x86_64-unknown-linux-gnu",
                    version: "1.2.3",
                    repo: None,
                    meta: PkgMeta {
                        pkg_fmt: Some(pkg_fmt),
                        ..Default::default()
                    },
                    bin_path: Path::new("/tmp/pkg/bin-mycrate"),
                    install_path: Path::new("/home/user/.cargo/bin"),
                    target_related_info: &NoValues,
                };
                let tt = Template::parse("mycrate-dir/{ bin }{ binary-ext }").unwrap();
                BinFile::new(&data, "mycrate", &tt, &mut |_| false, true, &[])
                    .unwrap()
                    .source
            };

            // The file the download was decompressed to is the binary, and
            // `bin-dir` is not used, since there is no archive to look in.
            for pkg_fmt in [
                PkgFmt::Bin,
                PkgFmt::Gz,
                PkgFmt::Bz2,
                PkgFmt::Xz,
                PkgFmt::Zst,
            ] {
                assert_eq!(
                    source(pkg_fmt),
                    Path::new("/tmp/pkg/bin-mycrate"),
                    "{pkg_fmt}"
                );
            }

            assert_eq!(
                source(PkgFmt::Tgz),
                Path::new("/tmp/pkg/bin-mycrate/mycrate-dir/mycrate")
            );
        }

        #[test]
        fn aliases_do_not_depend_on_the_versioned_link() {
            let bin = bin_file("x86_64-unknown-linux-gnu", false, &["mcr"]);

            assert_eq!(
                bin.link.as_deref(),
                Some(Path::new("/home/user/.cargo/bin/mycrate"))
            );
            assert_eq!(bin.dest, Path::new("/home/user/.cargo/bin/mycrate-v1.2.3"));
            assert_eq!(bin.aliases, [Path::new("/home/user/.cargo/bin/mcr")]);
            assert_eq!(
                bin.preview_link().to_string(),
                "mycrate (/home/user/.cargo/bin/mycrate -> mycrate-v1.2.3)"
            );
        }

        #[test]
        fn aliases_get_the_windows_binary_extension() {
            let bin = bin_file("x86_64-pc-windows-msvc", true, &["mcr", "mcr.v2"]);
            assert_eq!(bin.dest, Path::new("/home/user/.cargo/bin/mycrate.exe"));
            assert_eq!(
                bin.aliases,
                [
                    Path::new("/home/user/.cargo/bin/mcr.exe"),
                    // The extension is appended, not replacing `.v2`.
                    Path::new("/home/user/.cargo/bin/mcr.v2.exe"),
                ]
            );
        }

        #[test]
        fn aliases_must_not_clash() {
            let new_with = |name: &str, no_symlinks: bool, aliases: &[&str]| {
                let aliases: Vec<CompactString> = aliases.iter().copied().map(Into::into).collect();
                let data = Data {
                    name: "mycrate",
                    target: "x86_64-unknown-linux-gnu",
                    version: "1.2.3",
                    repo: None,
                    meta: PkgMeta::default(),
                    bin_path: Path::new("/tmp/pkg"),
                    install_path: Path::new("/home/user/.cargo/bin"),
                    target_related_info: &NoValues,
                };
                let tt = Template::parse("{ bin }{ binary-ext }").unwrap();
                BinFile::new(&data, name, &tt, &mut |_| false, no_symlinks, &aliases).unwrap()
            };
            let new = |name: &str, aliases: &[&str]| new_with(name, true, aliases);

            check_aliases_unique(&[new("a", &["x"]), new("b", &["y"])]).unwrap();

            for bin_files in [
                // An alias named like the versioned file of a binary
                [new_with("a", false, &["a-v1.2.3"]), new("b", &[])],
                // ... or like the link to it
                [new_with("a", false, &["x"]), new_with("b", false, &["a"])],
                // An alias of a binary named like another binary
                [new("a", &["b"]), new("b", &[])],
                // ... or like itself
                [new("a", &["a"]), new("b", &[])],
                // Two binaries with the same alias
                [new("a", &["x"]), new("b", &["x"])],
            ] {
                assert!(matches!(
                    check_aliases_unique(&bin_files),
                    Err(Error::AliasClash(_))
                ));
            }
        }

        #[test]
        fn aliases_must_be_plain_file_names() {
            for alias in ["", "..", "../mcr", "sub/mcr", "/usr/bin/mcr", "C:mcr"] {
                let res = new_bin_file(
                    Path::new("/tmp/pkg/mycrate"),
                    Path::new("/home/user/.cargo/bin"),
                    "x86_64-unknown-linux-gnu",
                    false,
                    &["mcr", alias],
                );
                assert!(
                    matches!(&res, Err(Error::InvalidBinName(name)) if name == alias),
                    "{alias:?}: {:?}",
                    res.map(|bin| bin.aliases)
                );
            }
        }

        /// Aliases are relative symlinks to the installed binary, so that the
        /// install dir can be moved or copied without breaking them.
        #[cfg(unix)]
        #[test]
        fn aliases_are_relative_symlinks() {
            for (no_symlinks, target) in [(true, "mycrate"), (false, "mycrate-v1.2.3")] {
                let tmp = tempfile::tempdir().unwrap();
                let pkg = tmp.path().join("pkg");
                let install = tmp.path().join("install");
                fs::create_dir(&pkg).unwrap();
                fs::create_dir(&install).unwrap();
                fs::write(pkg.join("mycrate"), b"bin").unwrap();

                let bin = new_bin_file(
                    &pkg.join("mycrate"),
                    &install,
                    "x86_64-unknown-linux-gnu",
                    no_symlinks,
                    &["mcr"],
                )
                .unwrap();
                bin.install_bin().unwrap();
                bin.install_link().unwrap();

                let alias = install.join("mcr");
                assert_eq!(fs::read_link(&alias).unwrap(), Path::new(target));
                if !no_symlinks {
                    assert_eq!(
                        fs::read_link(install.join("mycrate")).unwrap(),
                        Path::new(target)
                    );
                }
                assert_eq!(fs::read(&alias).unwrap(), b"bin");

                // Installing again over an existing alias is fine.
                bin.install_link().unwrap();
                assert_eq!(fs::read(&alias).unwrap(), b"bin");

                // But not without clobbering.
                bin.install_link_noclobber().unwrap_err();
            }
        }
    }

    mod bin_dir {
        use super::*;
        use binstalk_types::cargo_toml_binstall::PkgMeta;

        struct NoValues;
        impl leon::Values for NoValues {
            fn get_value(&self, _key: &str) -> Option<Cow<'_, str>> {
                None
            }
        }

        /// The archive source path of binary `mybin` with `bin-dir` set to
        /// `bin_dir`, in an archive whose only dirs are `dirs`.
        fn source_path(bin_dir: &str, target: &str, dirs: &[&str]) -> PathBuf {
            let data = Data {
                name: "mycrate",
                target,
                version: "1.2.3",
                repo: None,
                meta: PkgMeta::default(),
                bin_path: Path::new("/tmp/pkg"),
                install_path: Path::new("/home/user/.cargo/bin"),
                target_related_info: &NoValues,
            };
            let tt = Template::parse(bin_dir).unwrap();
            let mut has_dir = |p: &Path| dirs.iter().any(|dir| p == Path::new(dir));
            let bin = BinFile::new(&data, "mybin", &tt, &mut has_dir, true, &[]).unwrap();
            assert_eq!(
                bin.source,
                Path::new("/tmp/pkg").join(&bin.archive_source_path)
            );
            bin.archive_source_path
        }

        const LINUX: &str = "x86_64-unknown-linux-gnu";
        const WINDOWS: &str = "x86_64-pc-windows-msvc";

        #[test]
        fn file_path_is_used_as_is() {
            assert_eq!(
                source_path(
                    "{ name }-{ target }/{ bin }{ binary-ext }",
                    LINUX,
                    &["mycrate-x86_64-unknown-linux-gnu"]
                ),
                Path::new("mycrate-x86_64-unknown-linux-gnu/mybin")
            );
            assert_eq!(
                source_path("{ bin }-{ target }{ binary-ext }", WINDOWS, &[]),
                Path::new("mybin-x86_64-pc-windows-msvc.exe")
            );
        }

        #[test]
        fn dir_path_gets_the_bin_file_name() {
            for bin_dir in ["{ name }-{ target }", "{ name }-{ target }/"] {
                assert_eq!(
                    source_path(bin_dir, LINUX, &["mycrate-x86_64-unknown-linux-gnu"]),
                    Path::new("mycrate-x86_64-unknown-linux-gnu/mybin")
                );
            }
            assert_eq!(
                source_path("{ name }/bin", WINDOWS, &["mycrate", "mycrate/bin"]),
                Path::new("mycrate/bin/mybin.exe")
            );
        }

        /// A binary in a dir of the same name, e.g. `mybin/mybin`.
        #[test]
        fn dir_named_like_the_bin() {
            assert_eq!(
                source_path("{ bin }", LINUX, &["mybin"]),
                Path::new("mybin/mybin")
            );
        }

        #[test]
        fn missing_path_is_used_as_is() {
            assert_eq!(
                source_path("{ name }-{ target }", LINUX, &[]),
                Path::new("mycrate-x86_64-unknown-linux-gnu")
            );
        }
    }
}
