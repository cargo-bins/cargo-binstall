use std::{
    future::Future,
    process::{ExitCode, Termination},
    time::Duration,
};

use binstalk::errors::BinstallError;
use binstalk::helpers::tasks::AutoAbortJoinHandle;
use miette::Result;
use tokio::runtime::Runtime;
use tracing::{error, info};

use crate::signal::cancel_on_user_sig_term;

pub(crate) enum MainExit {
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
                error!("Fatal error:\n{err:?}");
                ExitCode::from(16)
            }
        }
    }
}

impl MainExit {
    pub(crate) fn new(res: Result<()>, done: Duration) -> Self {
        res.map(|()| MainExit::Success(Some(done)))
            .unwrap_or_else(|err| {
                err.downcast::<BinstallError>()
                    .map(MainExit::Error)
                    .unwrap_or_else(MainExit::Report)
            })
    }
}

/// This function would start a tokio multithreading runtime,
/// spawn a new task on it that runs `f()`, then `block_on` it.
///
/// It will cancel the future if user requested cancellation
/// via signal.
pub(crate) fn run_tokio_main<Func, Fut>(f: Func) -> Result<()>
where
    Func: FnOnce() -> Result<Option<Fut>>,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let rt = Runtime::new().map_err(BinstallError::from)?;
    let _guard = rt.enter();

    if let Some(fut) = f()? {
        let handle = AutoAbortJoinHandle::new(rt.spawn(fut));
        rt.block_on(cancel_on_user_sig_term(handle))?
    } else {
        Ok(())
    }
}
