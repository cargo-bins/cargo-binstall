use std::{
    cmp::min,
    io::{self, Write},
    iter::repeat,
};

use log::{LevelFilter, Log, STATIC_MAX_LEVEL};
use once_cell::sync::Lazy;
use supports_color::{on as supports_color_on_stream, Stream::Stdout};
use tracing::{
    callsite::Callsite,
    dispatcher, field,
    subscriber::{self, set_global_default},
    Event, Level, Metadata,
};
use tracing_core::{identify_callsite, metadata::Kind, subscriber::Subscriber};
use tracing_log::AsTrace;
use tracing_subscriber::{
    filter::targets::Targets,
    fmt::{
        fmt,
        format::{self, FormatEvent, FormatFields, Writer},
        FmtContext, MakeWriter,
    },
    layer::SubscriberExt,
    registry::LookupSpan,
};

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

fn loglevel_to_cs(level: log::Level) -> (&'static Fields, &'static Metadata<'static>) {
    match level {
        log::Level::Trace => (&*TRACE_FIELDS, &TRACE_META),
        log::Level::Debug => (&*DEBUG_FIELDS, &DEBUG_META),
        log::Level::Info => (&*INFO_FIELDS, &INFO_META),
        log::Level::Warn => (&*WARN_FIELDS, &WARN_META),
        log::Level::Error => (&*ERROR_FIELDS, &ERROR_META),
    }
}

struct Logger;

impl Logger {
    fn init(log_level: LevelFilter) {
        log::set_max_level(log_level);
        log::set_logger(&Self).unwrap();
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
                let (keys, meta) = loglevel_to_cs(record.level());

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

struct ErrorFreeWriter;

fn report_err(err: io::Error) {
    writeln!(io::stderr(), "Failed to write to stdout: {err}").ok();
}

impl io::Write for &ErrorFreeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf).or_else(|err| {
            report_err(err);
            // Behave as if writing to /dev/null so that logging system
            // would keep working.
            Ok(buf.len())
        })
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        io::stdout().write_all(buf).or_else(|err| {
            report_err(err);
            // Behave as if writing to /dev/null so that logging system
            // would keep working.
            Ok(())
        })
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        io::stdout().write_vectored(bufs).or_else(|err| {
            report_err(err);
            // Behave as if writing to /dev/null so that logging system
            // would keep working.
            Ok(bufs.iter().map(|io_slice| io_slice.len()).sum())
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush().or_else(|err| {
            report_err(err);
            // Behave as if writing to /dev/null so that logging system
            // would keep working.
            Ok(())
        })
    }
}

impl<'a> MakeWriter<'a> for ErrorFreeWriter {
    type Writer = &'a Self;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

struct HumanEventFormatter(format::Format<format::Full, ()>);

impl Default for HumanEventFormatter {
    fn default() -> Self {
        // Keep the existing compact human output while adding the program prefix.
        Self(
            format::format()
                .without_time()
                .with_target(false)
                .with_file(false)
                .with_line_number(false)
                .with_thread_names(false)
                .with_thread_ids(false),
        )
    }
}

impl<S, N> FormatEvent<S, N> for HumanEventFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        writer.write_str("cargo-binstall: ")?;
        self.0.format_event(ctx, writer, event)
    }
}

pub fn logging(log_level: LevelFilter, json_output: bool) {
    // Calculate log_level
    let log_level = min(log_level, STATIC_MAX_LEVEL);

    let allowed_targets = (log_level != LevelFilter::Trace).then_some([
        "atomic_file_install",
        "binstalk",
        "binstalk_bins",
        "binstalk_downloader",
        "binstalk_fetchers",
        "binstalk_registry",
        "cargo_binstall",
        "cargo_toml_workspace",
        "detect_targets",
        "simple_git",
    ]);

    // Forward log to tracing
    Logger::init(log_level);

    // Build fmt subscriber
    let log_level = log_level.as_trace();
    let subscriber_builder = fmt().with_max_level(log_level).with_writer(ErrorFreeWriter);

    let subscriber: Box<dyn Subscriber + Send + Sync> = if json_output {
        Box::new(subscriber_builder.json().finish())
    } else {
        // subscriber_builder defaults to write to io::stdout(),
        // so tests whether it supports color.
        let stdout_supports_color = supports_color_on_stream(Stdout)
            .map(|color_level| color_level.has_basic)
            .unwrap_or_default();

        Box::new(
            subscriber_builder
                .with_ansi(stdout_supports_color)
                .event_format(HumanEventFormatter::default())
                .finish(),
        )
    };

    // Builder layer for filtering
    let filter_layer = allowed_targets.map(|allowed_targets| {
        Targets::new().with_targets(allowed_targets.into_iter().zip(repeat(log_level)))
    });

    // Builder final subscriber with filtering
    let subscriber = subscriber.with(filter_layer);

    // Setup global subscriber
    set_global_default(subscriber).unwrap();
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use tracing::subscriber;

    use super::*;

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl SharedWriter {
        fn output(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn human_subscriber(writer: SharedWriter) -> impl Subscriber {
        fmt()
            .with_max_level(Level::TRACE)
            .with_writer(writer)
            .with_ansi(false)
            .event_format(HumanEventFormatter::default())
            .finish()
    }

    #[test]
    fn human_events_include_program_prefix() {
        let writer = SharedWriter::default();
        let subscriber = human_subscriber(writer.clone());

        subscriber::with_default(subscriber, || {
            tracing::info!("info message");
            tracing::warn!("warn message");
            tracing::error!("error message");
        });

        let output = writer.output();
        let mut lines = output.lines();
        let info = lines.next().unwrap();
        let warn = lines.next().unwrap();
        let error = lines.next().unwrap();

        assert!(info.starts_with("cargo-binstall:  INFO"));
        assert!(info.ends_with("info message"));
        assert!(warn.starts_with("cargo-binstall:  WARN"));
        assert!(warn.ends_with("warn message"));
        assert!(error.starts_with("cargo-binstall: ERROR"));
        assert!(error.ends_with("error message"));
        assert!(lines.next().is_none());
    }

    #[test]
    fn human_prefix_is_per_event() {
        let writer = SharedWriter::default();
        let subscriber = human_subscriber(writer.clone());

        subscriber::with_default(subscriber, || tracing::info!("first line\nsecond line"));

        assert_eq!(
            writer.output(),
            "cargo-binstall:  INFO first line\nsecond line\n"
        );
    }
}
