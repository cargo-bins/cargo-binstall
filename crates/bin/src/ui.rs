use std::{
    cmp::min,
    io::{self, BufRead, Write},
    thread,
};

use log::LevelFilter;
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tokio::sync::mpsc;

use binstalk::errors::BinstallError;

use crate::args::Args;

const IS_RELEASE_BUILD: bool = !cfg!(debug_assertions);

const MAX_LOG_LEVEL: LevelFilter = if IS_RELEASE_BUILD {
    if cfg!(feature = "log_release_max_level_info") {
        LevelFilter::Info
    } else if cfg!(feature = "log_release_max_level_debug") {
        LevelFilter::Debug
    } else {
        LevelFilter::Trace
    }
} else {
    LevelFilter::Trace
};

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
    let log_level = min(args.log_level, MAX_LOG_LEVEL);

    // Setup logging
    let mut log_config = ConfigBuilder::new();

    if log_level != LevelFilter::Trace {
        log_config.add_filter_allow_str("binstalk");
        log_config.add_filter_allow_str("cargo_binstall");
    }

    log_config.set_location_level(LevelFilter::Off);
    TermLogger::init(
        log_level,
        log_config.build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();
}
