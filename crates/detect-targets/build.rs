fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Fetch build target and define this for the compiler
    let target = std::env::var("TARGET").unwrap();

    print_env("TARGET", &target);
}

fn print_env(key: &str, val: &dyn std::fmt::Display) {
    println!("cargo:rustc-env={key}={val}");
}
