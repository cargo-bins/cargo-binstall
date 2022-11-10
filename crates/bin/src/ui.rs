use std::{
    cmp::min,
    io::{self, BufRead, Write},
    iter::repeat,
    thread,
};

use log::{LevelFilter, Log, STATIC_MAX_LEVEL};
use once_cell::sync::Lazy;
use tokio::sync::mpsc;
use tracing::{
    callsite::Callsite,
    dispatcher, field,
    subscriber::{self, set_global_default},
    Event, Level, Metadata,
};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_core::{identify_callsite, metadata::Kind};
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

struct Logger {
    inner: LogTracer,
    allowed_targets: Option<&'static [&'static str]>,
}

impl Logger {
    fn init(log_level: LevelFilter, allowed_targets: Option<&'static [&'static str]>) {
        log::set_max_level(log_level);
        log::set_boxed_logger(Box::new(Self {
            inner: Default::default(),
            allowed_targets,
        }))
        .unwrap();
    }
}

struct Fields {
    message: field::Field,
}

static FIELD_NAMES: &[&str] = &["message"];

impl Fields {
    fn new(cs: &'static dyn Callsite) -> Self {
        let fieldset = cs.metadata().fields();
        let message = fieldset.field("message").unwrap();
        Fields { message }
    }
}

macro_rules! log_cs {
    ($level:expr, $cs:ident, $meta:ident, $fields:ident, $ty:ident) => {
        struct $ty;
        static $cs: $ty = $ty;
        static $meta: Metadata<'static> = Metadata::new(
            "log event",
            "log",
            $level,
            None,
            None,
            None,
            field::FieldSet::new(FIELD_NAMES, identify_callsite!(&$cs)),
            Kind::EVENT,
        );
        static $fields: Lazy<Fields> = Lazy::new(|| Fields::new(&$cs));

        impl Callsite for $ty {
            fn set_interest(&self, _: subscriber::Interest) {}
            fn metadata(&self) -> &'static Metadata<'static> {
                &$meta
            }
        }
    };
}

log_cs!(
    Level::TRACE,
    TRACE_CS,
    TRACE_META,
    TRACE_FIELDS,
    TraceCallsite
);
log_cs!(
    Level::DEBUG,
    DEBUG_CS,
    DEBUG_META,
    DEBUG_FIELDS,
    DebugCallsite
);
log_cs!(Level::INFO, INFO_CS, INFO_META, INFO_FIELDS, InfoCallsite);
log_cs!(Level::WARN, WARN_CS, WARN_META, WARN_FIELDS, WarnCallsite);
log_cs!(
    Level::ERROR,
    ERROR_CS,
    ERROR_META,
    ERROR_FIELDS,
    ErrorCallsite
);

fn loglevel_to_cs(
    level: log::Level,
) -> (
    &'static dyn Callsite,
    &'static Fields,
    &'static Metadata<'static>,
) {
    match level {
        log::Level::Trace => (&TRACE_CS, &*TRACE_FIELDS, &TRACE_META),
        log::Level::Debug => (&DEBUG_CS, &*DEBUG_FIELDS, &DEBUG_META),
        log::Level::Info => (&INFO_CS, &*INFO_FIELDS, &INFO_META),
        log::Level::Warn => (&WARN_CS, &*WARN_FIELDS, &WARN_META),
        log::Level::Error => (&ERROR_CS, &*ERROR_FIELDS, &ERROR_META),
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.level() > log::max_level() {
            // First, check the log record against the current max level enabled.
            false
        } else if let Some(allowed_targets) = self.allowed_targets {
            // Keep only targets allowed

            for allowed_target in allowed_targets {
                // Use starts_with to emulate behavior of simplelog
                if metadata.target().starts_with(allowed_target) {
                    return true;
                }
            }

            false
        } else {
            true
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            dispatcher::get_default(|dispatch| {
                let filter_meta = record.as_trace();
                if !dispatch.enabled(&filter_meta) {
                    return;
                }

                let (_, keys, meta) = loglevel_to_cs(record.level());

                dispatch.event(&Event::new(
                    meta,
                    &meta
                        .fields()
                        .value_set(&[(&keys.message, Some(record.args() as &dyn field::Value))]),
                ));
            });
        }
    }

    fn flush(&self) {
        self.inner.flush()
    }
}

pub fn logging(args: &Args) -> WorkerGuard {
    // Calculate log_level
    let log_level = min(args.log_level, STATIC_MAX_LEVEL);

    let allowed_targets: Option<&[&str]> =
        (log_level != LevelFilter::Trace).then_some(&["binstalk", "cargo_binstall"]);

    // Forward log to tracing
    //
    // P.S. We omit the log filtering here since LogTracer does not
    // support `add_filter_allow_str`.
    Logger::init(log_level, allowed_targets);

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
        Targets::new().with_targets(allowed_targets.iter().copied().zip(repeat(log_level)))
    });

    // Builder final subscriber with filtering
    let subscriber = subscriber.with(filter_layer);

    // Setup global subscriber
    set_global_default(subscriber).unwrap();

    guard
}
