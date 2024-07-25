use std::{
    io, mem,
    path::{Path, PathBuf},
};

use cargo_toml::{Error as CargoTomlError, Manifest};
use compact_str::CompactString;
use glob::PatternError;
use normalize_path::NormalizePath;
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
    PatternError(#[from] PatternError),

    #[error("Invalid pattern `{0}`: It must be relative and point within current dir")]
    InvalidPatternError(CompactString),

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
        workspace_path.to_owned()
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

        if let Some(ws) = manifest.workspace {
            let excludes = ws.exclude;
            let members = ws.members;

            if members.is_empty() {
                continue;
            }

            let exclude_patterns = excludes
                .into_iter()
                .map(|pat| Pattern::new(&pat))
                .collect::<Result<Vec<_>, _>>()?;

            let workspace_path = manifest_path.parent().unwrap();

            for member in members {
                for path in Pattern::new(&member)?.glob_dirs(workspace_path)? {
                    if !exclude_patterns
                        .iter()
                        .any(|exclude| exclude.matches_with_trailing(&path))
                    {
                        manifest_paths.push(workspace_path.join(path).join("Cargo.toml"));
                    }
                }
            }
        }
    }

    Err(ErrorInner::NotFound)
}

struct Pattern(Vec<glob::Pattern>);

impl Pattern {
    fn new(pat: &str) -> Result<Self, ErrorInner> {
        Path::new(pat)
            .try_normalize()
            .ok_or_else(|| ErrorInner::InvalidPatternError(pat.into()))?
            .iter()
            .map(|c| glob::Pattern::new(c.to_str().unwrap()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
            .map(Self)
    }

    /// * `glob_path` - path to dir to glob for
    ///
    /// return paths relative to `glob_path`.
    fn glob_dirs(&self, glob_path: &Path) -> Result<Vec<PathBuf>, ErrorInner> {
        let mut paths = vec![PathBuf::new()];

        for pattern in &self.0 {
            if paths.is_empty() {
                break;
            }

            for path in mem::take(&mut paths) {
                let p = glob_path.join(&path);
                let res = p.read_dir();
                if res.is_err() && !p.is_dir() {
                    continue;
                }
                drop(p);

                for res in res? {
                    let entry = res?;

                    let is_dir = entry
                        .file_type()
                        .map(|file_type| file_type.is_dir() || file_type.is_symlink())
                        .unwrap_or(false);
                    if !is_dir {
                        continue;
                    }

                    let filename = entry.file_name();
                    if filename != "." // Ignore current dir
                        && filename != ".." // Ignore parent dir
                        && pattern.matches(&filename.to_string_lossy())
                    {
                        paths.push(path.join(filename));
                    }
                }
            }
        }

        Ok(paths)
    }

    /// Return `true` if `path` matches the pattern.
    /// It will still return `true` even if there are some trailing components.
    fn matches_with_trailing(&self, path: &Path) -> bool {
        let mut iter = path.iter().map(|os_str| os_str.to_string_lossy());
        for pattern in &self.0 {
            match iter.next() {
                Some(s) if pattern.matches(&s) => (),
                _ => return false,
            }
        }
        true
    }
}

#[cfg(test)]
mod test {
    use std::fs::create_dir_all as mkdir;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_glob_dirs() {
        let pattern = Pattern::new("*/*/q/*").unwrap();
        let tempdir = TempDir::new().unwrap();

        mkdir(tempdir.as_ref().join("a/b/c/efe")).unwrap();
        mkdir(tempdir.as_ref().join("a/b/q/ww")).unwrap();
        mkdir(tempdir.as_ref().join("d/233/q/d")).unwrap();

        let mut paths = pattern.glob_dirs(tempdir.as_ref()).unwrap();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![PathBuf::from("a/b/q/ww"), PathBuf::from("d/233/q/d")]
        );
    }

    #[test]
    fn test_matches_with_trailing() {
        let pattern = Pattern::new("*/*/q/*").unwrap();

        assert!(pattern.matches_with_trailing(Path::new("a/b/q/d/")));
        assert!(pattern.matches_with_trailing(Path::new("a/b/q/d")));
        assert!(pattern.matches_with_trailing(Path::new("a/b/q/d/234")));
        assert!(pattern.matches_with_trailing(Path::new("a/234/q/d/234")));

        assert!(!pattern.matches_with_trailing(Path::new("")));
        assert!(!pattern.matches_with_trailing(Path::new("a/")));
        assert!(!pattern.matches_with_trailing(Path::new("a/234")));
        assert!(!pattern.matches_with_trailing(Path::new("a/234/q")));
    }

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
        assert_eq!(package.version.as_ref().unwrap(), "0.12.0");
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
        assert_eq!(package.version.as_ref().unwrap(), "8.4.0");
        assert_eq!(manifest.bin.len(), 1);
        assert_eq!(manifest.bin[0].name.as_deref().unwrap(), "cargo-watch");
    }
}
