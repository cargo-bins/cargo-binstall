fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    embed_resource::compile("manifest.rc");
}
