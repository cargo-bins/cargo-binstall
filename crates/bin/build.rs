fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=manifest.rc");
    println!("cargo:rerun-if-changed=windows.manifest");

    embed_resource::compile("manifest.rc", embed_resource::NONE);
}
