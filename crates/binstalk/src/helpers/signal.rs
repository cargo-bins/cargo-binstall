use std::io;

use futures_util::future::pending;
use tokio::{signal, sync::OnceCell};

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
    #[cfg(unix)]
    unix::ignore_signals_on_unix()?;

    tokio::select! {
        res = handle => res,
        res = wait_on_cancellation_signal() => {
            res.and(Err(BinstallError::UserAbort))
        }
    }
}

/// If call to it returns `Ok(())`, then all calls to this function after
/// that also returns `Ok(())`.
pub async fn wait_on_cancellation_signal() -> Result<(), BinstallError> {
    static CANCELLED: OnceCell<()> = OnceCell::const_new();

    CANCELLED
        .get_or_try_init(wait_on_cancellation_signal_inner)
        .await
        .copied()
        .map_err(BinstallError::Io)
}

async fn wait_on_cancellation_signal_inner() -> Result<(), io::Error> {
    #[cfg(unix)]
    async fn inner() -> Result<(), io::Error> {
        unix::wait_on_cancellation_signal_unix().await
    }

    #[cfg(not(unix))]
    async fn inner() -> Result<(), io::Error> {
        // Use pending here so that tokio::select! would just skip this branch.
        pending().await
    }

    tokio::select! {
        res = signal::ctrl_c() => res,
        res = inner() => res,
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use signal::unix::{signal, SignalKind};

    /// Same as [`wait_on_cancellation_signal`] but is only available on unix.
    pub async fn wait_on_cancellation_signal_unix() -> Result<(), io::Error> {
        tokio::select! {
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
            pending().await
        }
    }

    pub fn ignore_signals_on_unix() -> Result<(), BinstallError> {
        drop(signal(SignalKind::user_defined1())?);
        drop(signal(SignalKind::user_defined2())?);

        Ok(())
    }
}
