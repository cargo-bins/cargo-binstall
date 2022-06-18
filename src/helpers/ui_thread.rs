use std::io::{self, BufRead, Write};

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use crate::BinstallError;

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
    request_tx: mpsc::Sender<UIRequest>,

    /// Confirmation
    confirm_rx: mpsc::Receiver<Result<(), BinstallError>>,
}

impl UIThreadInner {
    fn new() -> Self {
        let (request_tx, mut request_rx) = mpsc::channel(1);
        let (confirm_tx, confirm_rx) = mpsc::channel(10);

        spawn_blocking(move || {
            // This task should be the only one able to
            // access stdin
            let mut stdin = io::stdin().lock();
            let mut stdout = io::stdout().lock();
            let mut stderr = io::stderr().lock();
            let mut input = String::with_capacity(16);

            loop {
                match request_rx.blocking_recv() {
                    Some(UIRequest::Confirm) => {
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
                    Some(UIRequest::PrintToStdout(output)) => stdout.write_all(&output).unwrap(),
                    Some(UIRequest::PrintToStderr(output)) => stderr.write_all(&output).unwrap(),
                    Some(UIRequest::FlushStdout) => stdout.flush().unwrap(),
                    None => break,
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
            .await
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
    pub fn new(enable: bool) -> Self {
        let ui_thread = UIThreadInner::new();
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
