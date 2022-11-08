use binstalk_signal::{ignore_signals, wait_on_cancellation_signal};

use super::tasks::AutoAbortJoinHandle;
use crate::errors::BinstallError;

/// This function will poll the handle while listening for ctrl_c,
/// `SIGINT`, `SIGHUP`, `SIGTERM` and `SIGQUIT`.
///
/// When signal is received, [`BinstallError::UserAbort`] will be returned.
///
/// It would also ignore `SIGUSER1` and `SIGUSER2` on unix.
///
/// This function uses [`tokio::signal`] and once exit, does not reset the default
/// signal handler, so be careful when using it.
pub async fn cancel_on_user_sig_term<T>(
    handle: AutoAbortJoinHandle<T>,
) -> Result<T, BinstallError> {
    ignore_signals()?;

    tokio::select! {
        res = handle => res,
        res = wait_on_cancellation_signal() => {
            res.map_err(BinstallError::Io)
                .and(Err(BinstallError::UserAbort))
        }
    }
}
