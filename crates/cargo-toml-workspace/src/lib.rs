use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use cargo_toml::{Error as CargoTomlError, Manifest};
use compact_str::CompactString;
use glob::{MatchOptions, Pattern, PatternError};
use serde::de::DeserializeOwned;
use thiserror::Error as ThisError;
use tracing::{debug, instrument, warn};

pub use cargo_toml;

/// Load binstall metadata `Cargo.toml` from workspace at the provided path
///
/// WARNING: This is a blocking operation.
///
///  * `workspace_path` - can be a directory (path to workspace) or
///    a file (path to `Cargo.toml`).
pub fn load_manifest_from_workspace<Metadata: DeserializeOwned>(
    workspace_path: impl AsRef<Path>,
    crate_name: impl AsRef<str>,
) -> Result<Manifest<Metadata>, Error> {
    fn inner<Metadata: DeserializeOwned>(
        workspace_path: &Path,
        crate_name: &str,
    ) -> Result<Manifest<Metadata>, Error> {
        load_manifest_from_workspace_inner(workspace_path, crate_name).map_err(|inner| Error {
            workspace_path: workspace_path.into(),
            crate_name: crate_name.into(),
            inner,
        })
    }

    inner(workspace_path.as_ref(), crate_name.as_ref())
}

#[derive(Debug, ThisError)]
#[error("Failed to load {crate_name} from {}: {inner}", workspace_path.display())]
pub struct Error {
    workspace_path: Box<Path>,
    crate_name: CompactString,
    #[source]
    inner: ErrorInner,
}

#[derive(Debug, ThisError)]
enum ErrorInner {
    #[error("Invalid pattern in workspace.members or workspace.exclude: {0}")]
    Pattern(#[from] PatternError),

    #[error("Failed to parse cargo manifest: {0}")]
    CargoManifest(#[from] CargoTomlError),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Not found")]
    NotFound,
}

#[instrument]
fn load_manifest_from_workspace_inner<Metadata: DeserializeOwned>(
    workspace_path: &Path,
    crate_name: &str,
) -> Result<Manifest<Metadata>, ErrorInner> {
    debug!(
        "Loading manifest of crate {crate_name} from workspace: {}",
        workspace_path.display()
    );

    let manifest_path = if workspace_path.is_file() {
        if workspace_path.parent().unwrap() == Path::new("") {
            Path::new(&Component::CurDir).join(workspace_path)
        } else {
            workspace_path.to_owned()
        }
    } else {
        workspace_path.join("Cargo.toml")
    };

    let mut manifest_paths = vec![manifest_path];

    while let Some(manifest_path) = manifest_paths.pop() {
        let manifest = Manifest::<Metadata>::from_path_with_metadata(&manifest_path)?;

        let name = manifest.package.as_ref().map(|p| &*p.name);
        debug!(
            "Loading from {}, manifest.package.name = {:#?}",
            manifest_path.display(),
            name
        );

        if name == Some(crate_name) {
            return Ok(manifest);
        }

        let Some(ws) = manifest.workspace else {
            continue;
        };
        if ws.members.is_empty() {
            continue;
        }

        let root = manifest_path.parent().unwrap();
        // This follows `members_paths` and `is_excluded` in cargo's
        // `core/workspace.rs`. Patterns are matched relative to the root, so
        // the root itself may contain glob metacharacters and need not be
        // UTF-8.

        // As in cargo, an exclude entry excludes everything under that path,
        // unless it is also under a path listed in members.
        // Unlike cargo, and for backwards compatibility, an exclude entry may
        // also be a glob pattern. It is matched the same way as members, one
        // path component at a time, against each ancestor of the path.
        let exclude_patterns = ws
            .exclude
            .iter()
            .map(|exclude| Pattern::new(exclude))
            .collect::<Result<Vec<_>, PatternError>>()?;
        let is_excluded = |path: &Path| {
            let Ok(relative) = path.strip_prefix(root) else {
                return false;
            };
            let excluded = ws
                .exclude
                .iter()
                .any(|exclude| relative.starts_with(exclude))
                || relative.ancestors().any(|ancestor| {
                    exclude_patterns
                        .iter()
                        .any(|pattern| pattern.matches_path_with(ancestor, COMPONENT_WISE))
                });
            let explicit_member = ws.members.iter().any(|member| relative.starts_with(member));

            excluded && !explicit_member
        };

        for member in &ws.members {
            let mut paths = expand_member(root, member)?;
            // As in cargo, if the glob matches nothing, use the member as a
            // literal path.
            if paths.is_empty() {
                paths.push(root.join(member));
            }

            for mut path in paths {
                if !path.is_dir() || is_excluded(&path) {
                    continue;
                }
                path.push("Cargo.toml");
                if path.is_file() {
                    manifest_paths.push(path);
                }
            }
        }
    }

    Err(ErrorInner::NotFound)
}

/// `require_literal_separator`, so that `*` does not match across `/`, as when
/// a pattern is expanded one path component at a time.
const COMPONENT_WISE: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// Expand `pattern` relative to `root` into the directories it matches.
///
/// This gives the same result as `glob::glob(root.join(pattern))`, which is
/// what cargo uses, but `root` is never interpreted as a pattern and need not
/// be UTF-8.
fn expand_member(root: &Path, pattern: &str) -> Result<Vec<PathBuf>, ErrorInner> {
    let mut dirs = vec![root.to_owned()];

    let mut components = Path::new(pattern).components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            // `/`, `..`, `.` and Windows prefixes are taken literally.
            dirs = dirs.into_iter().map(|dir| dir.join(component)).collect();
            continue;
        };
        // `pattern` is a `str`, so all of its components are too.
        let component = component.to_str().unwrap();

        let mut next = Vec::new();
        if component == "**" {
            // Zero or more directories, but like `glob`, a trailing `**`
            // matches only what is below the directory, not the directory
            // itself.
            let include_self = components.peek().is_some();
            let mut todo = Vec::new();
            for dir in dirs.drain(..) {
                push_subdirs(&dir, None, &mut todo)?;
                if include_self {
                    next.push(dir);
                }
            }
            while let Some(dir) = todo.pop() {
                push_subdirs(&dir, None, &mut todo)?;
                next.push(dir);
            }
        } else if component.contains(['*', '?', '[']) {
            let pattern = Pattern::new(component)?;
            for dir in &dirs {
                push_subdirs(dir, Some(&pattern), &mut next)?;
            }
        } else {
            next.extend(
                dirs.iter()
                    .map(|dir| dir.join(component))
                    .filter(|dir| dir.is_dir()),
            );
        }
        dirs = next;
    }

    dirs.sort_unstable();
    dirs.dedup();
    Ok(dirs)
}

/// Push the subdirectories of `dir` whose name matches `pattern`, following
/// symlinks like `glob` does. Names that are not UTF-8 never match a pattern.
fn push_subdirs(
    dir: &Path,
    pattern: Option<&Pattern>,
    out: &mut Vec<PathBuf>,
) -> Result<(), ErrorInner> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let matches = match pattern {
            None => true,
            Some(pattern) => entry
                .file_name()
                .to_str()
                .is_some_and(|name| pattern.matches_with(name, glob::MatchOptions::new())),
        };
        if matches {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_load() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("e2e-tests/manifests/workspace");

        let manifest =
            load_manifest_from_workspace::<cargo_toml::Value>(&p, "cargo-binstall").unwrap();
        let package = manifest.package.unwrap();
        assert_eq!(package.name, "cargo-binstall");
        assert_eq!(package.version.as_ref().unwrap().to_string(), "0.12.0");
        assert_eq!(manifest.bin.len(), 1);
        assert_eq!(manifest.bin[0].name.as_deref().unwrap(), "cargo-binstall");
        assert_eq!(manifest.bin[0].path.as_deref().unwrap(), "src/main.rs");

        let err = load_manifest_from_workspace_inner::<cargo_toml::Value>(&p, "cargo-binstall2")
            .unwrap_err();
        assert!(matches!(err, ErrorInner::NotFound), "{:#?}", err);

        let manifest =
            load_manifest_from_workspace::<cargo_toml::Value>(&p, "cargo-watch").unwrap();
        let package = manifest.package.unwrap();
        assert_eq!(package.name, "cargo-watch");
        assert_eq!(package.version.as_ref().unwrap().to_string(), "8.4.0");
        assert_eq!(manifest.bin.len(), 1);
        assert_eq!(manifest.bin[0].name.as_deref().unwrap(), "cargo-watch");
    }

    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let dst = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &dst);
            } else {
                std::fs::copy(entry.path(), dst).unwrap();
            }
        }
    }

    /// The workspace root itself must not be interpreted as a glob pattern.
    #[test]
    fn test_load_root_with_glob_metachars() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("e2e-tests/manifests/workspace");

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("ws[1]*?");
        copy_dir(&src, &p);

        let manifest =
            load_manifest_from_workspace::<cargo_toml::Value>(&p, "cargo-watch").unwrap();
        assert_eq!(manifest.package.unwrap().name, "cargo-watch");

        let err = load_manifest_from_workspace_inner::<cargo_toml::Value>(&p, "cargo-binstall2")
            .unwrap_err();
        assert!(matches!(err, ErrorInner::NotFound), "{:#?}", err);
    }

    fn write_package(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
    }

    fn find(root: &Path, name: &str) -> bool {
        match load_manifest_from_workspace_inner::<cargo_toml::Value>(root, name) {
            Ok(manifest) => {
                assert_eq!(manifest.package.unwrap().name, name);
                true
            }
            Err(ErrorInner::NotFound) => false,
            Err(err) => panic!("{err:#?}"),
        }
    }

    /// Cases where cargo's `members_paths` and `is_excluded` behave
    /// differently from a gitignore-style walk.
    #[test]
    fn test_cargo_member_semantics() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
members = ["top", "excluded/explicit", "excluded/*", "lit[1]"]
exclude = ["excluded"]
"#,
        )
        .unwrap();

        write_package(&root.join("top"), "top");
        // A member without a slash is relative to the root, it does not
        // match a directory of the same name further down.
        write_package(&root.join("nested/top"), "nested-top");
        // Members listed explicitly win over exclude...
        write_package(&root.join("excluded/explicit"), "explicit");
        // ...but globs do not.
        write_package(&root.join("excluded/globbed"), "globbed");
        // A member that matches nothing as a glob is used as a literal path.
        write_package(&root.join("lit[1]"), "literal");

        assert!(find(root, "top"));
        assert!(!find(root, "nested-top"));
        assert!(find(root, "explicit"));
        assert!(!find(root, "globbed"));
        assert!(find(root, "literal"));
    }

    /// `expand_member` must agree with `glob::glob`, which cargo uses.
    #[test]
    fn test_expand_member_matches_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in [
            "crates/a/b/c",
            "crates/x/.hidden/y",
            "crates/x[1]/z",
            "b/c/d",
            ".dot/e",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("crates/file"), "").unwrap();

        let escaped_root = Pattern::escape(root.to_str().unwrap());
        for pattern in [
            "crates/*",
            "crates/**",
            "crates/**/*",
            "**",
            "**/c",
            "*",
            "crates/?",
            "crates/[ab]",
            "crates/x[[]1]",
            "crates/*/.hidden",
            "crates/*/*/y",
            "b/c/d",
            "./b/../b/c",
            "missing/*",
            "missing",
        ] {
            let mut expected = glob::glob(&format!("{escaped_root}/{pattern}"))
                .unwrap()
                .map(Result::unwrap)
                .filter(|path| path.is_dir())
                .collect::<Vec<_>>();
            expected.sort_unstable();

            assert_eq!(
                expand_member(root, pattern).unwrap(),
                expected,
                "pattern {pattern}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_load_non_utf8_root() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("e2e-tests/manifests/workspace");

        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(OsStr::from_bytes(b"ws\xff"));
        copy_dir(&src, &p);
        assert!(p.to_str().is_none());

        let manifest =
            load_manifest_from_workspace::<cargo_toml::Value>(&p, "cargo-watch").unwrap();
        assert_eq!(manifest.package.unwrap().name, "cargo-watch");

        let err = load_manifest_from_workspace_inner::<cargo_toml::Value>(&p, "cargo-binstall2")
            .unwrap_err();
        assert!(matches!(err, ErrorInner::NotFound), "{:#?}", err);
    }
}
