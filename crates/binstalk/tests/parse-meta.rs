use binstalk::ops::resolve::load_manifest_path;
use cargo_toml_workspace::cargo_toml::{Edition, Product};
use std::path::PathBuf;

#[test]
fn parse_meta() {
    let mut manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    manifest_dir.push("tests/parse-meta.Cargo.toml");

    let manifest =
        load_manifest_path(&manifest_dir, "cargo-binstall-test").expect("Error parsing metadata");
    let package = manifest.package.unwrap();
    let meta = package.metadata.and_then(|m| m.binstall).unwrap();

    assert_eq!(&package.name, "cargo-binstall-test");

    assert_eq!(
        meta.pkg_url.as_deref().unwrap(),
        "{ repo }/releases/download/v{ version }/{ name }-{ target }.{ archive-format }"
    );

    assert_eq!(
        manifest.bin.as_slice(),
        &[Product {
            name: Some("cargo-binstall".to_string()),
            path: Some("src/main.rs".to_string()),
            edition: Some(Edition::E2021),
            ..Default::default()
        },],
    );
}
