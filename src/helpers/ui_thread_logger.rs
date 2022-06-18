use std::cell::Cell;
use std::fmt::Write;

use bytes::BytesMut;
use log::{set_boxed_logger, set_max_level, LevelFilter, Log, Metadata, Record};
use tokio::{runtime::Handle, sync::mpsc::Sender};

use super::ui_thread::UIRequest;

#[derive(Debug)]
pub(super) struct UIThreadLogger {
    tx: Sender<UIRequest>,
    level: LevelFilter,
    filter_ignore: &'static [&'static str],
}

impl UIThreadLogger {
    pub(super) fn init(
        tx: Sender<UIRequest>,
        level: LevelFilter,
        filter_ignore: &'static [&'static str],
    ) {
        set_max_level(level);
        set_boxed_logger(Self::new(tx, level, filter_ignore)).unwrap()
    }

    fn new(
        tx: Sender<UIRequest>,
        level: LevelFilter,
        filter_ignore: &'static [&'static str],
    ) -> Box<Self> {
        Box::new(Self {
            tx,
            level,
            filter_ignore,
        })
    }

    fn send_request(&self, request: UIRequest) {
        // TODO: Use another mpsc type.
        // Tokio's mpsc requires the async send to be used
        // in async context.
        if let Ok(handle) = Handle::try_current() {
            let tx = self.tx.clone();
            handle.spawn(async move { tx.send(request).await.unwrap() });
        } else {
            self.tx.blocking_send(request).unwrap();
        }
    }

    thread_local! {
        static BUFFER: Cell<BytesMut> = Cell::new(BytesMut::new());
    }
}

impl Log for UIThreadLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        let target = record.target();
        if self.enabled(record.metadata())
            && !self
                .filter_ignore
                .iter()
                .any(|filter| target.starts_with(filter))
        {
            let output = Self::BUFFER.with(|cell| {
                let mut buffer = cell.take();
                write!(&mut buffer, "{}", record.args()).unwrap();

                let output = buffer.split().freeze();
                cell.set(buffer);

                output
            });

            self.send_request(UIRequest::PrintToStdout(output));
        }
    }

    fn flush(&self) {
        self.send_request(UIRequest::FlushStdout);
    }
}
