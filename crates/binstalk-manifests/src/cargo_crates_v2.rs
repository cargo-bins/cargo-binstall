//! Cargo's `.crates2.json` manifest.
//!
//! Cargo keeps this file next to `.crates.toml` and records, for each
//! installed package, the options it was built with (features, profile,
//! target, ...). `.crates.toml` stays the source of truth: every time cargo
//! opens the two files, it copies any change made to `.crates.toml` into
//! `.crates2.json`, filling in defaults for packages it knows nothing about.
//!
//! Binstall does not build anything, so it records the same defaults cargo
//! would have filled in itself. This keeps `.crates2.json` in step with
//! `.crates.toml` for tools that read it directly, without claiming anything
//! about how a prebuilt binary was built.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Seek},
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    cargo_crates_v1::{CratesToml, package_id},
    crate_info::CrateInfo,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CratesV2 {
    /// Package id to install info.
    ///
    /// The info is kept as raw JSON so that entries binstall does not touch
    /// are written back exactly as cargo left them, whatever version of cargo
    /// wrote them.
    installs: BTreeMap<String, Map<String, Value>>,

    /// Unknown keys from other versions of cargo, kept as-is.
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// What cargo records for a package it only knows from `.crates.toml`, see
/// `InstallInfo::from_v1` in cargo.
#[derive(Serialize)]
struct InstallInfo<'a> {
    version_req: Option<&'a str>,
    bins: BTreeSet<&'a str>,
    features: [&'a str; 0],
    all_features: bool,
    no_default_features: bool,
    profile: &'a str,
    target: Option<&'a str>,
    rustc: Option<&'a str>,
}

impl InstallInfo<'_> {
    fn from_v1<'a>(bins: impl IntoIterator<Item = &'a str>) -> Map<String, Value> {
        let info = InstallInfo {
            version_req: None,
            bins: bins.into_iter().collect(),
            features: [],
            all_features: false,
            no_default_features: false,
            profile: "release",
            target: None,
            rustc: None,
        };
        match serde_json::to_value(info) {
            Ok(Value::Object(map)) => map,
            _ => unreachable!("InstallInfo always serialises to a JSON object"),
        }
    }
}

impl CratesV2 {
    pub fn load_from_reader<R: io::Read>(mut reader: R) -> Result<Self, CratesV2ParseError> {
        fn inner(reader: &mut dyn io::Read) -> Result<CratesV2, CratesV2ParseError> {
            let mut vec = Vec::new();
            reader.read_to_end(&mut vec)?;

            if vec.is_empty() {
                Ok(CratesV2::default())
            } else {
                Ok(serde_json::from_slice(&vec)?)
            }
        }

        inner(&mut reader)
    }

    pub fn write_to_file(&self, file: &mut File) -> Result<(), CratesV2ParseError> {
        file.rewind()?;
        serde_json::to_writer(&mut *file, self)?;
        let pos = file.stream_position()?;
        file.set_len(pos)?;

        Ok(())
    }

    /// Record `crates` as freshly installed by binstall, and bring the rest
    /// of the manifest in line with `v1`, the `.crates.toml` that was just
    /// written.
    ///
    /// This is what cargo would do the next time it opened both files, except
    /// that options recorded by an earlier `cargo install` of the same package
    /// id are dropped: they describe a binary binstall just replaced.
    pub(crate) fn update(&mut self, crates: &[CrateInfo], v1: &CratesToml<'_>) {
        // Forget what is recorded for these package ids, so `sync_v1` gives
        // them the defaults.
        for metadata in crates {
            self.installs.remove(&package_id(metadata));
        }
        self.sync_v1(v1);
    }

    /// Same as cargo's `CrateListingV2::sync_v1`: `bins` are copied from
    /// `.crates.toml`, packages missing here get the defaults, and packages
    /// no longer in `.crates.toml` are removed.
    fn sync_v1(&mut self, v1: &CratesToml<'_>) {
        let mut installs = BTreeMap::new();

        for (id, bins) in v1.entries() {
            let bins = || bins.iter().map(|bin| bin.as_str());
            let info = match self.installs.remove(id) {
                Some(mut info) => {
                    let bins = bins().collect::<BTreeSet<_>>();
                    info.insert("bins".into(), bins.into_iter().map(Value::from).collect());
                    info
                }
                None => InstallInfo::from_v1(bins()),
            };
            installs.insert(id.to_owned(), info);
        }

        self.installs = installs;
    }
}

#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum CratesV2ParseError {
    #[error("I/O Error: {0}")]
    Io(#[from] io::Error),

    #[error("Failed to (de)serialize json: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crate_info::CrateSource;

    use semver::Version;
    use serde_json::json;

    fn crate_info(name: &str, version: Version, bins: &[&str]) -> CrateInfo {
        CrateInfo {
            name: name.into(),
            version_req: "*".into(),
            current_version: version,
            source: CrateSource::cratesio_registry(),
            target: "x86_64-unknown-linux-musl".into(),
            bins: bins.iter().map(|bin| (*bin).into()).collect(),
            pkg_url_hint: None,
        }
    }

    fn v1(data: &str) -> CratesToml<'static> {
        CratesToml::load_from_reader(data.as_bytes()).unwrap()
    }

    const FROM_V1: fn(&[&str]) -> Value = |bins| {
        json!({
            "version_req": null,
            "bins": bins,
            "features": [],
            "all_features": false,
            "no_default_features": false,
            "profile": "release",
            "target": null,
            "rustc": null,
        })
    };

    #[test]
    fn update_matches_cargo_sync() {
        let mut v2 = CratesV2::load_from_reader(
            json!({
                "installs": {
                    // Installed by cargo, still in v1: kept untouched.
                    "cargo-watch 8.5.3 (registry+https://github.com/rust-lang/crates.io-index)": {
                        "version_req": null,
                        "bins": ["cargo-watch"],
                        "features": ["x"],
                        "all_features": false,
                        "no_default_features": true,
                        "profile": "release",
                        "target": "x86_64-unknown-linux-gnu",
                        "rustc": "rustc 1.97.0",
                        "future_field": 1,
                    },
                    // Older version replaced by binstall: removed.
                    "sccache 0.17.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                        "version_req": null,
                        "bins": ["sccache"],
                        "features": [],
                        "all_features": false,
                        "no_default_features": false,
                        "profile": "release",
                        "target": "x86_64-unknown-linux-gnu",
                        "rustc": "rustc 1.97.0",
                    },
                    // Same id reinstalled by binstall: options reset.
                    "ripgrep 14.1.1 (registry+https://github.com/rust-lang/crates.io-index)": {
                        "version_req": "14",
                        "bins": ["rg"],
                        "features": ["pcre2"],
                        "all_features": false,
                        "no_default_features": false,
                        "profile": "release",
                        "target": "x86_64-unknown-linux-gnu",
                        "rustc": "rustc 1.97.0",
                    },
                    // Uninstalled by an old cargo that only knows v1: removed.
                    "gone 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)": {
                        "version_req": null,
                        "bins": ["gone"],
                        "features": [],
                        "all_features": false,
                        "no_default_features": false,
                        "profile": "release",
                        "target": null,
                        "rustc": null,
                    },
                },
                "future_top_level": true,
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();

        let crates = [
            crate_info("sccache", Version::new(0, 18, 0), &["sccache"]),
            crate_info("ripgrep", Version::new(14, 1, 1), &["rg"]),
        ];
        let mut v1_file = v1(r#"
[v1]
"cargo-watch 8.5.3 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-watch"]
"old-cargo 2.0.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["b", "a"]
"#);
        for metadata in &crates {
            v1_file.add_crate(metadata);
        }

        v2.update(&crates, &v1_file);

        assert_eq!(
            serde_json::to_value(&v2).unwrap(),
            json!({
                "installs": {
                    "cargo-watch 8.5.3 (registry+https://github.com/rust-lang/crates.io-index)": {
                        "version_req": null,
                        "bins": ["cargo-watch"],
                        "features": ["x"],
                        "all_features": false,
                        "no_default_features": true,
                        "profile": "release",
                        "target": "x86_64-unknown-linux-gnu",
                        "rustc": "rustc 1.97.0",
                        "future_field": 1,
                    },
                    // Installed by an old cargo that only knows v1: defaults.
                    "old-cargo 2.0.0 (registry+https://github.com/rust-lang/crates.io-index)":
                        FROM_V1(&["a", "b"]),
                    "sccache 0.18.0 (registry+https://github.com/rust-lang/crates.io-index)":
                        FROM_V1(&["sccache"]),
                    "ripgrep 14.1.1 (registry+https://github.com/rust-lang/crates.io-index)":
                        FROM_V1(&["rg"]),
                },
                "future_top_level": true,
            })
        );
    }

    #[test]
    fn empty() {
        let mut v2 = CratesV2::load_from_reader(&b""[..]).unwrap();
        let crates = [crate_info(
            "cargo-binstall",
            Version::new(1, 0, 0),
            &["cargo-binstall"],
        )];
        let mut v1_file = CratesToml::default();
        v1_file.add_crate(&crates[0]);

        v2.update(&crates, &v1_file);

        assert_eq!(
            serde_json::to_value(&v2).unwrap(),
            json!({
                "installs": {
                    "cargo-binstall 1.0.0 (registry+https://github.com/rust-lang/crates.io-index)":
                        FROM_V1(&["cargo-binstall"]),
                },
            })
        );
    }
}
