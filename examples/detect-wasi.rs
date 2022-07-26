use std::process::exit;

use cargo_binstall::wasi::detect_wasi_runability;

fn main() {
    if detect_wasi_runability().unwrap() {
        println!("WASI is runnable!");
        exit(0);
    } else {
        println!("WASI is not runnable");
        exit(1);
    }
}
