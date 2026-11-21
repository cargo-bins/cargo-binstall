use std::{
    io,
    path::{Component, Path, PathBuf},
};

use cargo_toml::{Error as CargoTomlError, Manifest};
use compact_str::CompactString;
use glob::{GlobError, Pattern, PatternError};
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

    #[error("Failed to walk directory: {0}")]
    Walk(#[from] GlobError),

    #[error("Workspace path {} is not valid UTF-8", .0.display())]
    NonUtf8Path(PathBuf),

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
        // Patterns are matched against full paths, so escape the root in case
        // it contains glob metacharacters such as `[`.
        let escaped_root = Pattern::escape(
            root.to_str()
                .ok_or_else(|| ErrorInner::NonUtf8Path(root.to_owned()))?,
        );
        let pattern_at_root = |pattern: &str| format!("{escaped_root}/{pattern}");

        // Like cargo, an exclude entry excludes everything under that path.
        // For backwards compatibility, it may also be a glob pattern.
        let excludes = ws
            .exclude
            .iter()
            .map(|exclude| Ok((root.join(exclude), Pattern::new(&pattern_at_root(exclude))?)))
            .collect::<Result<Vec<_>, PatternError>>()?;
        let is_excluded = |path: &Path| {
            excludes
                .iter()
                .any(|(prefix, pattern)| path.starts_with(prefix) || pattern.matches_path(path))
        };

        for member in &ws.members {
            for res in glob::glob(&pattern_at_root(member))? {
                let mut path = res?;
                if is_excluded(&path) {
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
}
