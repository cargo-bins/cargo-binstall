use std::time::Instant;

use binstalk::helpers::jobserver_client::LazyJobserverClient;
use tracing::debug;

use cargo_binstall::{
    args,
    bin_util::{run_tokio_main, MainExit},
    entry,
    logging::logging,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> MainExit {
    // This must be the very first thing to happen
    let jobserver_client = LazyJobserverClient::new();

    let args = args::parse();

    if args.version {
        println!("{}", env!("CARGO_PKG_VERSION"));
        MainExit::Success(None)
    } else {
        logging(args.log_level, args.json_output);

        let start = Instant::now();

        let result = run_tokio_main(entry::install_crates(args, jobserver_client));

        let done = start.elapsed();
        debug!("run time: {done:?}");

        MainExit::new(result, done)
    }
}
