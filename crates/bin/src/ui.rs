use std::{
    cmp::min,
    io::{self, BufRead, Write},
    iter::repeat,
    thread,
};

use log::{LevelFilter, STATIC_MAX_LEVEL};
use tokio::sync::mpsc;
use tracing::subscriber::set_global_default;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_log::{log_tracer::LogTracer, AsTrace};
use tracing_subscriber::{filter::targets::Targets, fmt::fmt, layer::SubscriberExt};

use binstalk::errors::BinstallError;

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

pub fn logging(args: &Args) -> WorkerGuard {
    // Calculate log_level
    let log_level = min(args.log_level, STATIC_MAX_LEVEL);

    let allowed_targets =
        (log_level != LevelFilter::Trace).then_some(["binstalk", "cargo_binstall"]);

    // Forward log to tracing
    //
    // P.S. We omit the log filtering here since LogTracer does not
    // support `add_filter_allow_str`.
    LogTracer::builder()
        .with_max_level(log_level)
        .init()
        .unwrap();

    // Setup non-blocking stdout
    let (non_blocking, guard) = NonBlockingBuilder::default()
        .lossy(false)
        .finish(io::stdout());

    // Build fmt subscriber
    let log_level = log_level.as_trace();
    let subscriber = fmt()
        .with_writer(non_blocking)
        .with_max_level(log_level)
        .compact()
        // Disable time, target, file, line_num, thread name/ids to make the
        // output more readable
        .without_time()
        .with_target(false)
        .with_file(false)
        .with_line_number(false)
        .with_thread_names(false)
        .with_thread_ids(false)
        .finish();

    // Builder layer for filtering
    let filter_layer = allowed_targets.map(|allowed_targets| {
        Targets::new().with_targets(allowed_targets.into_iter().zip(repeat(log_level)))
    });

    // Builder final subscriber with filtering
    let subscriber = subscriber.with(filter_layer);

    // Setup global subscriber
    set_global_default(subscriber).unwrap();

    guard
}
