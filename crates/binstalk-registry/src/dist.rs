//! The config of [dist] a crate's workspace root has, for the fetchers to
//! find where dist uploads its releases.
//!
//! A published crate's `Cargo.toml` only keeps `[package.metadata.dist]`:
//! cargo drops `[workspace]` when it normalizes the manifest. The `.crate`
//! of a single-package workspace still has the rest, in `Cargo.toml.orig`
//! and in `dist-workspace.toml` or `dist.toml` next to it. That of a
//! workspace member doesn't: the root is outside the package.
//!
//! [dist]: https://github.com/axodotdev/cargo-dist

use std::{fs, path::Path};

use binstalk_types::cargo_toml_binstall::{DistMeta, Meta};
use cargo_toml_workspace::cargo_toml::Manifest;
use tracing::debug;

/// The files dist reads its config from, besides `Cargo.toml`, in
/// increasing precedence.
pub(crate) const DIST_CONFIG_FILES: [&str; 2] = ["dist-workspace.toml", "dist.toml"];

/// Upper bound on the size of a config file worth reading.
pub(crate) const MAX_CONFIG_FILE_SIZE: u64 = 1024 * 1024;

/// The dist config of a workspace root.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceDistConfig {
    /// The root's `Cargo.toml`, as written: `Cargo.toml.orig` in a `.crate`.
    pub(crate) root_manifest: Option<Vec<u8>>,
    /// The contents of each of [`DIST_CONFIG_FILES`] that exists.
    pub(crate) files: [Option<Vec<u8>>; DIST_CONFIG_FILES.len()],
}

impl WorkspaceDistConfig {
    /// Read the config of the workspace root `dir`, a crate's directory.
    fn read_dir(dir: &Path) -> Self {
        let read = |name: &str| {
            let path = dir.join(name);
            let metadata = fs::metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > MAX_CONFIG_FILE_SIZE {
                return None;
            }
            fs::read(&path)
                .inspect_err(|err| debug!("Failed to read {}: {err}", path.display()))
                .ok()
        };
        Self {
            root_manifest: read("Cargo.toml"),
            files: DIST_CONFIG_FILES.map(read),
        }
    }

    /// The settings found, the more preferred ones first, see
    /// <https://axodotdev.github.io/cargo-dist/book/reference/config.html#config>.
    fn dist_meta(&self) -> Option<DistMeta> {
        let tables = self
            .files
            .iter()
            .rev()
            .zip(DIST_CONFIG_FILES.iter().rev())
            .map(|(file, name)| (file.as_deref(), *name, &["dist"][..]))
            .chain([(
                self.root_manifest.as_deref(),
                "Cargo.toml",
                &["workspace", "metadata", "dist"][..],
            )]);

        tables
            .filter_map(|(file, name, path)| dist_table(file?, name, path))
            .reduce(DistMeta::or)
    }

    /// Merge the workspace's settings into the package's
    /// `[package.metadata.dist]`, which take precedence.
    pub(crate) fn merge_into(&self, manifest: &mut Manifest<Meta>) {
        let Some(workspace) = self.dist_meta() else {
            return;
        };
        let Some(package) = manifest.package.as_mut() else {
            return;
        };
        let meta = package.metadata.get_or_insert(Meta {
            binstall: None,
            dist: None,
        });
        meta.dist = Some(match meta.dist.take() {
            Some(dist) => dist.or(workspace),
            None => workspace,
        });
    }
}

/// The table at `path` in toml file `name`, if it is one.
fn dist_table(file: &[u8], name: &str, path: &[&str]) -> Option<DistMeta> {
    let parsed = std::str::from_utf8(file)
        .ok()
        .and_then(|file| file.parse::<toml::Table>().ok());
    let Some(table) = parsed else {
        debug!("Ignoring {name}: not valid toml");
        return None;
    };
    let (last, rest) = path.split_last()?;
    let mut table = &table;
    for key in rest {
        table = table.get(*key)?.as_table()?;
    }
    let value = table.get(*last)?;
    value
        .clone()
        .try_into()
        .inspect_err(|err| debug!("Ignoring the dist config in {name}: {err}"))
        .ok()
}

/// Merge the dist config of the workspace root `dir` into `manifest`, of
/// a crate in it, see [`WorkspaceDistConfig`].
///
/// This is a blocking function.
pub fn merge_workspace_dist_meta(dir: &Path, manifest: &mut Manifest<Meta>) {
    WorkspaceDistConfig::read_dir(dir).merge_into(manifest);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_manifest(package_dist: &str) -> Manifest<Meta> {
        Manifest::from_slice_with_metadata(
            format!(
                r#"
[package]
name = "foo"
version = "1.0.0"
{package_dist}
"#
            )
            .as_bytes(),
        )
        .unwrap()
    }

    fn dist_of(manifest: &Manifest<Meta>) -> Option<&DistMeta> {
        manifest.package.as_ref()?.metadata.as_ref()?.dist.as_ref()
    }

    #[test]
    fn precedence() {
        let config = WorkspaceDistConfig {
            root_manifest: Some(
                br#"
[package]
name = "foo"
version = "1.0.0"

[workspace.metadata.dist]
hosting = ["github"]
simple-download-url = "https://old.example.com/{tag}"
dist = false
"#
                .to_vec(),
            ),
            files: [
                Some(
                    br#"
[workspace]
members = ["cargo:."]

[dist]
hosting = ["simple", "github"]
"#
                    .to_vec(),
                ),
                Some(
                    b"[dist]\nsimple-download-url = \"https://static.example.com/{tag}\"\n"
                        .to_vec(),
                ),
            ],
        };

        let mut manifest = parse_manifest("[package.metadata.dist]\ndist = true\n");
        config.merge_into(&mut manifest);
        assert_eq!(
            dist_of(&manifest),
            Some(&DistMeta {
                dist: Some(true),
                hosting: Some(vec!["simple".into(), "github".into()]),
                simple_download_url: Some("https://static.example.com/{tag}".into()),
            })
        );

        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest).unwrap().dist, Some(false));
    }

    #[test]
    fn invalid_config_is_ignored() {
        let config = WorkspaceDistConfig {
            root_manifest: Some(b"[workspace.metadata]\ndist = 1\n".to_vec()),
            files: [Some(b"not toml [".to_vec()), Some(b"dist = 1\n".to_vec())],
        };
        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), None);

        // A table with only invalid settings is kept, as an empty one.
        let config = WorkspaceDistConfig {
            files: [Some(b"[dist]\nhosting = 1\n".to_vec()), None],
            ..Default::default()
        };
        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), Some(&DistMeta::default()));
    }

    #[test]
    fn no_config() {
        let mut manifest = parse_manifest("");
        WorkspaceDistConfig::default().merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), None);
    }
}
