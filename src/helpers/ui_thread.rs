use std::io::{self, BufRead, Write};

use tokio::sync::mpsc;
use tokio::task::spawn_blocking;

use crate::{binstall::Resolution, BinstallError};

#[derive(Debug)]
pub struct UIThread {
    /// Request for confirmation
    request_tx: mpsc::Sender<()>,

    /// Confirmation
    confirm_rx: mpsc::Receiver<Result<(), BinstallError>>,
}

impl UIThread {
    pub fn new() -> Self {
        let (request_tx, mut request_rx) = mpsc::channel(1);
        let (confirm_tx, confirm_rx) = mpsc::channel(10);

        spawn_blocking(move || {
            // This task should be the only one able to
            // access stdin
            let mut stdin = io::stdin().lock();
            let mut input = String::with_capacity(16);

            loop {
                if request_rx.blocking_recv().is_none() {
                    break;
                }

                // Lock stdout so that nobody can interfere
                // with confirmation.
                let mut stdout = io::stdout().lock();

                let res = loop {
                    writeln!(&mut stdout, "Do you wish to continue? yes/[no]").unwrap();
                    write!(&mut stdout, "? ").unwrap();
                    stdout.flush().unwrap();

                    input.clear();
                    stdin.read_line(&mut input).unwrap();

                    match input.as_str().trim() {
                        "yes" | "y" | "YES" | "Y" => break Ok(()),
                        "no" | "n" | "NO" | "N" | "" => break Err(BinstallError::UserAbort),
                        _ => continue,
                    }
                };

                confirm_tx
                    .blocking_send(res)
                    .expect("entry exits when confirming request");
            }
        });

        Self {
            request_tx,
            confirm_rx,
        }
    }

    pub async fn confirm(&mut self, _resolutions: &[Resolution], show_prompt: bool) -> Result<(), BinstallError> {
        if show_prompt {
        self.request_tx
            .send(())
            .await
            .map_err(|_| BinstallError::UserAbort)?;

        self.confirm_rx
            .recv()
            .await
            .unwrap_or(Err(BinstallError::UserAbort))
        } else {
            Ok(())
        }
    }

    pub fn start(&self) {
        todo!("start timer")
    }

    pub fn stop(&self) {
        todo!("stop timer")
    }

    pub fn setup(&self, steps: usize, bar_name: &str) {
        todo!("setup {steps}-step progress bar for {bar_name}")
    }

    pub fn step(&self, step: &str) {
        todo!("advance current progress bar by one with status {step}")
    }
}
