use std::{
    io::{self, BufRead, Write},
    thread,
};

use log::LevelFilter;
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tokio::sync::mpsc;

use binstall::errors::BinstallError;

use crate::args::Args;

#[derive(Debug)]
struct UIThreadInner {
    /// Request for confirmation
    request_tx: mpsc::Sender<()>,

    /// Confirmation
    confirm_rx: mpsc::Receiver<Result<(), BinstallError>>,
}

impl UIThreadInner {
    fn new() -> Self {
        let (request_tx, mut request_rx) = mpsc::channel(1);
        let (confirm_tx, confirm_rx) = mpsc::channel(10);

        thread::spawn(move || {
            // This task should be the only one able to
            // access stdin
            let mut stdin = io::stdin().lock();
            let mut input = String::with_capacity(16);

            loop {
                if request_rx.blocking_recv().is_none() {
                    break;
                }

                let res = loop {
                    {
                        let mut stdout = io::stdout().lock();

                        writeln!(&mut stdout, "Do you wish to continue? yes/[no]").unwrap();
                        write!(&mut stdout, "? ").unwrap();
                        stdout.flush().unwrap();
                    }

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

    async fn confirm(&mut self) -> Result<(), BinstallError> {
        self.request_tx
            .send(())
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
        Self(if enable {
            Some(UIThreadInner::new())
        } else {
            None
        })
    }

    pub async fn confirm(&mut self) -> Result<(), BinstallError> {
        if let Some(inner) = self.0.as_mut() {
            inner.confirm().await
        } else {
            Ok(())
        }
    }
}

pub fn logging(args: &Args) {
    // Setup logging
    let mut log_config = ConfigBuilder::new();
    log_config.add_filter_ignore("hyper".to_string());
    log_config.add_filter_ignore("reqwest".to_string());
    log_config.add_filter_ignore("rustls".to_string());
    log_config.set_location_level(LevelFilter::Off);
    TermLogger::init(
        args.log_level,
        log_config.build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();
}
