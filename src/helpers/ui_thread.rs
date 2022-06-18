use std::io::{self, BufRead, Write};

use bytes::Bytes;
use log::LevelFilter;
use std::sync::mpsc as mpsc_sync;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use super::ui_thread_logger::UIThreadLogger;
use crate::BinstallError;

#[derive(Debug)]
pub(super) enum UIRequest {
    /// Request user confirmation
    Confirm,
    /// Print to stdout
    PrintToStdout(Bytes),
    /// Print to stderr
    PrintToStderr(Bytes),
    /// Flush stdout
    FlushStdout,
}

#[derive(Debug)]
struct UIThreadInner {
    /// Request for confirmation
    request_tx: mpsc_sync::SyncSender<UIRequest>,

    /// Confirmation
    confirm_rx: mpsc::Receiver<Result<(), BinstallError>>,
}

impl UIThreadInner {
    fn new() -> Self {
        // Set it to a large enough number so it will never block.
        let (request_tx, request_rx) = mpsc_sync::sync_channel(50);
        let (confirm_tx, confirm_rx) = mpsc::channel(10);

        spawn_blocking(move || {
            // This task should be the only one able to
            // access stdin
            let mut stdin = io::stdin().lock();
            let mut stdout = io::stdout().lock();
            let mut input = String::with_capacity(16);

            loop {
                match request_rx.recv() {
                    Ok(UIRequest::Confirm) => {
                        let res = loop {
                            writeln!(&mut stdout, "Do you wish to continue? yes/[no]").unwrap();
                            write!(&mut stdout, "? ").unwrap();
                            stdout.flush().unwrap();

                            input.clear();
                            stdin.read_line(&mut input).unwrap();

                            match input.as_str().trim() {
                                "yes" | "y" | "YES" | "Y" => break Ok(()),
                                "no" | "n" | "NO" | "N" | "" => {
                                    break Err(BinstallError::UserAbort)
                                }
                                _ => continue,
                            }
                        };

                        confirm_tx
                            .blocking_send(res)
                            .expect("entry exits when confirming request")
                    }
                    Ok(UIRequest::PrintToStdout(output)) => stdout.write_all(&output).unwrap(),
                    Ok(UIRequest::PrintToStderr(output)) => {
                        io::stderr().write_all(&output).unwrap()
                    }
                    Ok(UIRequest::FlushStdout) => stdout.flush().unwrap(),
                    Err(_) => break,
                }
            }
        });

        Self {
            request_tx,
            confirm_rx,
        }
    }

    async fn confirm(&mut self) -> Result<(), BinstallError> {
        self.request_tx
            .send(UIRequest::Confirm)
            .map_err(|_| BinstallError::UserAbort)?;

        self.confirm_rx
            .recv()
            .await
            .unwrap_or(Err(BinstallError::UserAbort))
    }
}

#[derive(Debug)]
pub struct UIThread(Option<UIThreadInner>);

impl UIThread {
    ///  * `enable` - `true` to enable confirmation, `false` to disable it.
    pub fn new(enable: bool, level: LevelFilter, filter_ignore: &'static [&'static str]) -> Self {
        let ui_thread = UIThreadInner::new();
        UIThreadLogger::init(ui_thread.request_tx.clone(), level, filter_ignore);
        Self(enable.then(|| ui_thread))
    }

    pub async fn confirm(&mut self) -> Result<(), BinstallError> {
        if let Some(inner) = self.0.as_mut() {
            inner.confirm().await
        } else {
            Ok(())
        }
    }
}
