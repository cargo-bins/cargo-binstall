use std::time::Instant;

use binstall_lib::helpers::jobserver_client::LazyJobserverClient;
use log::debug;

use cargo_binstall::{
    args,
    bin_util::{run_tokio_main, MainExit},
    entry, ui,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> MainExit {
    // This must be the very first thing to happen
    let jobserver_client = LazyJobserverClient::new();

    let args = match args::parse() {
        Ok(args) => args,
        Err(err) => return MainExit::Error(err),
    };

    ui::logging(&args);

    let start = Instant::now();

    let result = run_tokio_main(entry::install_crates(args, jobserver_client));

    let done = start.elapsed();
    debug!("run time: {done:?}");

    MainExit::new(result, done)
}
