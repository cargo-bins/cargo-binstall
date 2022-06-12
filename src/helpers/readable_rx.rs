use std::cmp::min;
use std::io::{self, BufRead, Read};

use bytes::{Buf, Bytes};
use tokio::sync::mpsc::Receiver;

use super::async_extracter::Content;

#[derive(Debug)]
pub(crate) struct ReadableRx {
    rx: Receiver<Content>,
    bytes: Bytes,
}

impl ReadableRx {
    pub(crate) fn new(rx: Receiver<Content>) -> Self {
        Self {
            rx,
            bytes: Bytes::new(),
        }
    }
}

impl Read for ReadableRx {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.fill_buf()?.is_empty() {
            return Ok(0);
        }

        let bytes = &mut self.bytes;

        // copy_to_slice requires the bytes to have enough remaining bytes
        // to fill buf.
        let n = min(buf.len(), bytes.remaining());

        bytes.copy_to_slice(&mut buf[..n]);

        Ok(n)
    }
}

impl BufRead for ReadableRx {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = &mut self.bytes;
        if !bytes.has_remaining() {
            match self.rx.blocking_recv() {
                Some(Content::Data(new_bytes)) => *bytes = new_bytes,
                Some(Content::Abort) => {
                    return Err(io::Error::new(io::ErrorKind::Other, "Aborted"))
                }
                None => (),
            }
        }
        Ok(&*bytes)
    }

    fn consume(&mut self, amt: usize) {
        self.bytes.advance(amt);
    }
}
