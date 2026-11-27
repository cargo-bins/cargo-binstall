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

/// Upper bound on the size of a config file worth reading.
pub(crate) const MAX_CONFIG_FILE_SIZE: u64 = 1024 * 1024;

/// The dist config of a workspace root: the files dist reads it from.
#[derive(Debug, Default)]
pub(crate) struct WorkspaceDistConfig {
    /// The root's `Cargo.toml`, as written: `Cargo.toml.orig` in a `.crate`.
    cargo_toml: Option<Vec<u8>>,
    dist_workspace_toml: Option<Vec<u8>>,
    dist_toml: Option<Vec<u8>>,
}

impl WorkspaceDistConfig {
    /// Where to keep file `name` of a `.crate`, if it is one of the
    /// config's files.
    pub(crate) fn file_in_crate(&mut self, name: &Path) -> Option<&mut Option<Vec<u8>>> {
        match name.to_str()? {
            "Cargo.toml.orig" => Some(&mut self.cargo_toml),
            "dist-workspace.toml" => Some(&mut self.dist_workspace_toml),
            "dist.toml" => Some(&mut self.dist_toml),
            _ => None,
        }
    }

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
            cargo_toml: read("Cargo.toml"),
            dist_workspace_toml: read("dist-workspace.toml"),
            dist_toml: read("dist.toml"),
        }
    }

    /// The settings found, the more preferred ones first, see
    /// <https://axodotdev.github.io/cargo-dist/book/reference/config.html#config>.
    fn dist_meta(&self) -> Option<DistMeta> {
        [
            (&self.dist_toml, "dist.toml", &["dist"][..]),
            (
                &self.dist_workspace_toml,
                "dist-workspace.toml",
                &["dist"][..],
            ),
            (
                &self.cargo_toml,
                "Cargo.toml",
                &["workspace", "metadata", "dist"][..],
            ),
        ]
        .into_iter()
        .filter_map(|(file, name, path)| dist_table(file.as_deref()?, name, path))
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

/// Whether `file` may have dist's config: any of its tables or keys spells
/// `dist`. Every crate has a `Cargo.toml.orig`, so that only the few with
/// dist's config are kept and parsed.
pub(crate) fn may_have_dist_config(file: &[u8]) -> bool {
    file.windows(4).any(|window| window == b"dist")
}

/// The table at `path` in toml file `name`, if it is one.
fn dist_table(file: &[u8], name: &str, path: &[&str]) -> Option<DistMeta> {
    if !may_have_dist_config(file) {
        return None;
    }
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

/// Merge the [dist](https://github.com/axodotdev/cargo-dist) config of the
/// workspace root `dir` into the `[package.metadata.dist]` of `manifest`, a
/// crate in it: `[workspace.metadata.dist]` of `dir/Cargo.toml`, and the
/// `[dist]` of `dir/dist-workspace.toml` and `dir/dist.toml`.
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
            cargo_toml: Some(
                br#"
[package]
name = "foo"
version = "1.0.0"

[workspace.metadata.dist]
unix-archive = ".tar.zst"
simple-download-url = "https://old.example.com/{tag}"
dist = false
"#
                .to_vec(),
            ),
            dist_workspace_toml: Some(
                br#"
[workspace]
members = ["cargo:."]

[dist]
unix-archive = ".tar.gz"
"#
                .to_vec(),
            ),
            dist_toml: Some(
                b"[dist]\nsimple-download-url = \"https://static.example.com/{tag}\"\n".to_vec(),
            ),
        };

        let mut manifest = parse_manifest("[package.metadata.dist]\ndist = true\n");
        config.merge_into(&mut manifest);
        assert_eq!(
            dist_of(&manifest),
            Some(&DistMeta {
                dist: Some(true),
                simple_download_url: Some("https://static.example.com/{tag}".into()),
                unix_archive: Some(".tar.gz".into()),
                windows_archive: None,
            })
        );

        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest).unwrap().dist, Some(false));
    }

    #[test]
    fn invalid_config_is_ignored() {
        let config = WorkspaceDistConfig {
            cargo_toml: Some(b"[workspace.metadata]\ndist = 1\n".to_vec()),
            dist_workspace_toml: Some(b"not toml [ dist".to_vec()),
            dist_toml: Some(b"dist = 1\n".to_vec()),
        };
        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), None);

        // A table with only invalid settings is kept, as an empty one.
        let config = WorkspaceDistConfig {
            dist_workspace_toml: Some(b"[dist]\nunix-archive = 1\n".to_vec()),
            ..Default::default()
        };
        let mut manifest = parse_manifest("");
        config.merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), Some(&DistMeta::default()));
    }

    #[test]
    fn files_in_crate() {
        let mut config = WorkspaceDistConfig::default();
        for name in ["Cargo.toml.orig", "dist-workspace.toml", "dist.toml"] {
            *config.file_in_crate(Path::new(name)).unwrap() =
                Some(b"[dist]\nunix-archive = \".tar.gz\"\n".to_vec());
        }
        assert!(config.file_in_crate(Path::new("Cargo.toml")).is_none());
        assert!(config.file_in_crate(Path::new("src/dist.toml")).is_none());
        assert_eq!(config.cargo_toml, config.dist_toml);
        assert_eq!(config.dist_workspace_toml, config.dist_toml);
    }

    #[test]
    fn no_config() {
        let mut manifest = parse_manifest("");
        WorkspaceDistConfig::default().merge_into(&mut manifest);
        assert_eq!(dist_of(&manifest), None);
    }
}
