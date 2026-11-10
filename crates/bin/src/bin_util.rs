use std::{
    process::{ExitCode, Termination},
    time::Duration,
};

use binstalk::{
    errors::{exit_code, Error, Exit, Result, ResultExt},
    helpers::tasks::AutoAbortJoinHandle,
};
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

use crate::{report::Report, signal::cancel_on_user_sig_term};

pub enum MainExit {
    Success(Option<Duration>),
    Error(Error),
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
            Self::Error(err) => {
                let code = exit_code(&err);

                if err.downcast_ref::<Exit>() == Some(&Exit::UserAbort) {
                    warn!("Installation cancelled");
                } else {
                    error!("Fatal error:");
                    println!("{:?}", miette::Report::new(Report(err)));
                }

                code
            }
        }
    }
}

impl MainExit {
    pub fn new(res: Result<()>, done: Option<Duration>) -> Self {
        res.map(|()| MainExit::Success(done))
            .unwrap_or_else(MainExit::Error)
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
    let rt = Runtime::new().or_raise(|| Exit::Io)?;
    let _guard = rt.enter();

    if let Some(handle) = f()? {
        rt.block_on(cancel_on_user_sig_term(handle))?
    } else {
        Ok(())
    }
}
