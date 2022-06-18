use std::cell::Cell;
use std::fmt::Write;
use std::sync::mpsc::SyncSender;

use bytes::BytesMut;
use log::{set_boxed_logger, set_max_level, LevelFilter, Log, Metadata, Record};

use super::ui_thread::UIRequest;

#[derive(Debug)]
pub(super) struct UIThreadLogger {
    tx: SyncSender<UIRequest>,
    level: LevelFilter,
    filter_ignore: &'static [&'static str],
}

impl UIThreadLogger {
    pub(super) fn init(
        tx: SyncSender<UIRequest>,
        level: LevelFilter,
        filter_ignore: &'static [&'static str],
    ) {
        set_max_level(level);
        set_boxed_logger(Self::new(tx, level, filter_ignore)).unwrap()
    }

    fn new(
        tx: SyncSender<UIRequest>,
        level: LevelFilter,
        filter_ignore: &'static [&'static str],
    ) -> Box<Self> {
        Box::new(Self {
            tx,
            level,
            filter_ignore,
        })
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

            self.tx.send(UIRequest::PrintToStdout(output)).unwrap()
        }
    }

    fn flush(&self) {
        self.tx.send(UIRequest::FlushStdout).unwrap();
    }
}
