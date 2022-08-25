fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Fetch build target and define this for the compiler
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
