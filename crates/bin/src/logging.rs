use std::{cmp::min, io, iter::repeat};

use log::{LevelFilter, Log, STATIC_MAX_LEVEL};
use once_cell::sync::Lazy;
use tracing::{
    callsite::Callsite,
    dispatcher, field,
    subscriber::{self, set_global_default},
    Event, Level, Metadata,
};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_core::{identify_callsite, metadata::Kind, subscriber::Subscriber};
use tracing_log::AsTrace;
use tracing_subscriber::{filter::targets::Targets, fmt::fmt, layer::SubscriberExt};

use crate::args::Args;

// Shamelessly taken from tracing-log

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

struct Logger;

impl Logger {
    fn init(log_level: LevelFilter) {
        log::set_max_level(log_level);
        log::set_boxed_logger(Box::new(Self)).unwrap();
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        if metadata.level() > log::max_level() {
            // First, check the log record against the current max level enabled.
            false
        } else {
            // Check if the current `tracing` dispatcher cares about this.
            dispatcher::get_default(|dispatch| dispatch.enabled(&metadata.as_trace()))
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        // Dispatch manually instead of using methods provided by tracing-log
        // to avoid having fields "log.target = ..." in the log message,
        // which makes the log really hard to read.
        if self.enabled(record.metadata()) {
            dispatcher::get_default(|dispatch| {
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

    fn flush(&self) {}
}

pub fn logging(args: &Args) -> WorkerGuard {
    // Calculate log_level
    let log_level = min(args.log_level, STATIC_MAX_LEVEL);

    let allowed_targets =
        (log_level != LevelFilter::Trace).then_some(["binstalk", "cargo_binstall"]);

    // Forward log to tracing
    Logger::init(log_level);

    // Setup non-blocking stdout
    let (non_blocking, guard) = NonBlockingBuilder::default()
        .lossy(false)
        .finish(io::stdout());

    // Build fmt subscriber
    let log_level = log_level.as_trace();
    let subscriber_builder = fmt()
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
        .with_thread_ids(false);

    let subscriber: Box<dyn Subscriber + Send + Sync> = if args.json_output {
        Box::new(subscriber_builder.json().finish())
    } else {
        Box::new(subscriber_builder.finish())
    };

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
