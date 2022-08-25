use std::{
    process::{ExitCode, Termination},
    time::{Duration, Instant},
};

use binstall::{
    errors::BinstallError,
    helpers::{
        jobserver_client::LazyJobserverClient, signal::cancel_on_user_sig_term,
        tasks::AutoAbortJoinHandle,
    },
};
use log::{debug, error, info};
use tokio::runtime::Runtime;

use cargo_binstall::*;

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

enum MainExit {
    Success(Duration),
    Error(BinstallError),
    Report(miette::Report),
}

impl Termination for MainExit {
    fn report(self) -> ExitCode {
        match self {
            Self::Success(spent) => {
                info!("Done in {spent:?}");
                ExitCode::SUCCESS
            }
            Self::Error(err) => err.report(),
            Self::Report(err) => {
                error!("Fatal error:");
                eprintln!("{err:?}");
                ExitCode::from(16)
            }
        }
    }
}
