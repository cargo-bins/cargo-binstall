use std::{
    future::Future,
    io::{self, Write},
    process::{ExitCode, Termination},
    time::Duration,
};

use binstalk::errors::BinstallError;
use binstalk::helpers::{signal::cancel_on_user_sig_term, tasks::AutoAbortJoinHandle};
use miette::Result;
use tokio::runtime::Runtime;

pub enum MainExit {
    Success(Option<Duration>),
    Error(BinstallError),
    Report(miette::Report),
}

impl Termination for MainExit {
    fn report(self) -> ExitCode {
        match self {
            Self::Success(spent) => {
                if let Some(spent) = spent {
                    writeln!(io::stdout(), "Done in {spent:?}").ok();
                }
                ExitCode::SUCCESS
            }
            Self::Error(err) => err.report(),
            Self::Report(err) => {
                writeln!(io::stderr(), "Fatal error:\n{err:?}").ok();
                ExitCode::from(16)
            }
        }
    }
}

impl MainExit {
    pub fn new(result: Result<Result<()>, BinstallError>, done: Duration) -> Self {
        result.map_or_else(MainExit::Error, |res| {
            res.map(|()| MainExit::Success(Some(done)))
                .unwrap_or_else(|err| {
                    err.downcast::<BinstallError>()
                        .map(MainExit::Error)
                        .unwrap_or_else(MainExit::Report)
                })
        })
    }
}

/// This function would start a tokio multithreading runtime,
/// spawn a new task on it that runs `f`, then `block_on` it.
///
/// It will cancel the future if user requested cancellation
/// via signal.
pub fn run_tokio_main<F, T>(f: F) -> Result<T, BinstallError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let rt = Runtime::new()?;
    let handle = AutoAbortJoinHandle::new(rt.spawn(f));
    rt.block_on(cancel_on_user_sig_term(handle))
}
