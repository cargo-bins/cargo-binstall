use std::io::{self, BufRead, Read};

use bytes::{Buf, Bytes};
use tokio::sync::mpsc;

/// This wraps an AsyncIterator as a `Read`able.
/// It must be used in non-async context only,
/// meaning you have to use it with
/// `tokio::task::{block_in_place, spawn_blocking}` or
/// `std::thread::spawn`.
pub struct StreamReadable {
    rx: mpsc::Receiver<Bytes>,
    bytes: Bytes,
}

impl StreamReadable {
    pub(super) fn new(rx: mpsc::Receiver<Bytes>) -> Self {
        Self {
            rx,
            bytes: Bytes::new(),
        }
    }
}

impl Read for StreamReadable {
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
        let n = buf.len().min(bytes.remaining());

        // <Bytes as Buf>::copy_to_slice copies and consumes the bytes
        bytes.copy_to_slice(&mut buf[..n]);

        Ok(n)
    }
}

impl BufRead for StreamReadable {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = &mut self.bytes;

        if !bytes.has_remaining() {
            if let Some(new_bytes) = self.rx.blocking_recv() {
                // new_bytes are guaranteed to be non-empty.
                *bytes = new_bytes;
            }
        }

        Ok(&*bytes)
    }

    fn consume(&mut self, amt: usize) {
        self.bytes.advance(amt);
    }
}
