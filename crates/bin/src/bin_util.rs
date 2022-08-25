use std::{
    process::{ExitCode, Termination},
    time::Duration,
};

use binstall::errors::BinstallError;
use log::{error, info};
use miette::Result;

pub enum MainExit {
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

impl MainExit {
    pub fn new(result: Result<Result<()>, BinstallError>, done: Duration) -> Self {
        result.map_or_else(MainExit::Error, |res| {
            res.map(|()| MainExit::Success(done)).unwrap_or_else(|err| {
                err.downcast::<BinstallError>()
                    .map(MainExit::Error)
                    .unwrap_or_else(MainExit::Report)
            })
        })
    }
}
