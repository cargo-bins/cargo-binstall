use std::process::Termination;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> impl Termination {
    cargo_binstall::main_impl()
}
