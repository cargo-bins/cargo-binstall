use std::process::Termination;

use cargo_binstall::do_main;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> impl Termination {
    do_main()
}
