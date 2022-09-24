use binstalk::ops::resolve::load_manifest_path;
use cargo_toml::Product;

#[test]
fn parse_meta() {
    let _ = env_logger::builder().is_test(true).try_init();

    let mut manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    manifest_dir.push_str("/tests/parse-meta.Cargo.toml");

    let manifest = load_manifest_path(&manifest_dir).expect("Error parsing metadata");
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
            edition: cargo_toml::Edition::E2021,
            ..Default::default()
        },],
    );
}
