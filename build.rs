fn main() {
    embed_resource::compile("manifest.rc");

    // Fetch build target and define this for the compiler
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
