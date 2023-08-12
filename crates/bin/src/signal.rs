use std::io;

use binstalk::{errors::BinstallError, helpers::tasks::AutoAbortJoinHandle};
use tokio::signal;

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
        biased;

        res = wait_on_cancellation_signal() => {
            res.map_err(BinstallError::Io)
                .and(Err(BinstallError::UserAbort))
        }
        res = handle => res,
    }
}

fn ignore_signals() -> io::Result<()> {
    #[cfg(unix)]
    unix::ignore_signals_on_unix()?;

    Ok(())
}

/// If call to it returns `Ok(())`, then all calls to this function after
/// that also returns `Ok(())`.
async fn wait_on_cancellation_signal() -> Result<(), io::Error> {
    #[cfg(unix)]
    unix::wait_on_cancellation_signal_unix().await?;

    #[cfg(not(unix))]
    signal::ctrl_c().await?;

    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use signal::unix::{signal, SignalKind};

    /// Same as [`wait_on_cancellation_signal`] but is only available on unix.
    pub async fn wait_on_cancellation_signal_unix() -> Result<(), io::Error> {
        tokio::select! {
            biased;

            res = wait_for_signal_unix(SignalKind::interrupt()) => res,
            res = wait_for_signal_unix(SignalKind::hangup()) => res,
            res = wait_for_signal_unix(SignalKind::terminate()) => res,
            res = wait_for_signal_unix(SignalKind::quit()) => res,
        }
    }

    /// Wait for first arrival of signal.
    pub async fn wait_for_signal_unix(kind: signal::unix::SignalKind) -> Result<(), io::Error> {
        let mut sig_listener = signal::unix::signal(kind)?;
        if sig_listener.recv().await.is_some() {
            Ok(())
        } else {
            // Use pending() here for the same reason as above.
            std::future::pending().await
        }
    }

    pub fn ignore_signals_on_unix() -> Result<(), io::Error> {
        drop(signal(SignalKind::user_defined1())?);
        drop(signal(SignalKind::user_defined2())?);

        Ok(())
    }
}
