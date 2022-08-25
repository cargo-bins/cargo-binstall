use std::time::Instant;

use binstall::{
    errors::BinstallError,
    helpers::{
        jobserver_client::LazyJobserverClient, signal::cancel_on_user_sig_term,
        tasks::AutoAbortJoinHandle,
    },
};
use log::debug;
use tokio::runtime::Runtime;

use cargo_binstall::{args, bin_util::MainExit, entry, ui};

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

    let result = {
        let rt = Runtime::new().unwrap();
        let handle =
            AutoAbortJoinHandle::new(rt.spawn(entry::install_crates(args, jobserver_client)));
        rt.block_on(cancel_on_user_sig_term(handle))
    };

    let done = start.elapsed();
    debug!("run time: {done:?}");

    result.map_or_else(MainExit::Error, |res| {
        res.map(|()| MainExit::Success(done)).unwrap_or_else(|err| {
            err.downcast::<BinstallError>()
                .map(MainExit::Error)
                .unwrap_or_else(MainExit::Report)
        })
    })
}
