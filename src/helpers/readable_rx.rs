use std::cmp::min;
use std::io::{self, Read};

use bytes::{Buf, Bytes};
use tokio::sync::mpsc::Receiver;

use super::async_file_writer::Content;

#[derive(Debug)]
pub(crate) struct ReadableRx<'a> {
    rx: &'a mut Receiver<Content>,
    bytes: Bytes,
}

impl<'a> ReadableRx<'a> {
    pub(crate) fn new(rx: &'a mut Receiver<Content>) -> Self {
        Self {
            rx,
            bytes: Bytes::new(),
        }
    }
}

impl Read for ReadableRx<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let bytes = &mut self.bytes;
        if !bytes.has_remaining() {
            match self.rx.blocking_recv() {
                Some(Content::Data(new_bytes)) => *bytes = new_bytes,
                Some(Content::Abort) => {
                    return Err(io::Error::new(io::ErrorKind::Other, "Aborted"))
                }
                None => return Ok(0),
            }
        }

        // copy_to_slice requires the bytes to have enough remaining bytes
        // to fill buf.
        let n = min(buf.len(), bytes.remaining());

        bytes.copy_to_slice(&mut buf[..n]);

        Ok(n)
    }
}
