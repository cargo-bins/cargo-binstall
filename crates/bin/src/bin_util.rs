use std::{
    process::{ExitCode, Termination},
    time::Duration,
};

use binstalk::errors::BinstallError;
use binstalk::helpers::tasks::AutoAbortJoinHandle;
use miette::Result;
use tokio::runtime::Runtime;
use tracing::{error, info};

use crate::signal::cancel_on_user_sig_term;

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
                    info!("Done in {spent:?}");
                }
                ExitCode::SUCCESS
            }
            Self::Error(err) => err.report(),
            Self::Report(err) => {
                error!("Fatal error:");
                println!("{err:?}");
                ExitCode::from(16)
            }
        }
    }
}

impl MainExit {
    pub fn new(res: Result<()>, done: Option<Duration>) -> Self {
        res.map(|()| MainExit::Success(done)).unwrap_or_else(|err| {
            err.downcast::<BinstallError>()
                .map(MainExit::Error)
                .unwrap_or_else(MainExit::Report)
        })
    }
}

/// This function would start a tokio multithreading runtime,
/// then `block_on` the task it returns.
///
/// It will cancel the future if user requested cancellation
/// via signal.
pub fn run_tokio_main(
    f: impl FnOnce() -> Result<Option<AutoAbortJoinHandle<Result<()>>>>,
) -> Result<()> {
    let rt = Runtime::new().map_err(BinstallError::from)?;
    let _guard = rt.enter();

    if let Some(handle) = f()? {
        rt.block_on(cancel_on_user_sig_term(handle))?
    } else {
        Ok(())
    }
}
