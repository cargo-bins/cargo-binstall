fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    embed_resource::compile("manifest.rc");
}
