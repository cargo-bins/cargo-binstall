use std::cmp::min;
use std::io::{self, BufRead, Read};

use bytes::{Buf, Bytes};
use futures_util::stream::{Stream, StreamExt};
use tokio::runtime::Handle;

use super::BinstallError;

/// This wraps an AsyncIterator as a `Read`able.
/// It must be used in non-async context only,
/// meaning you have to use it with
/// `tokio::task::{block_in_place, spawn_blocking}` or
/// `std::thread::spawn`.
#[derive(Debug)]
pub(super) struct StreamReadable<S> {
    stream: S,
    handle: Handle,
    bytes: Bytes,
}

impl<S> StreamReadable<S> {
    pub(super) async fn new(stream: S) -> Self {
        Self {
            stream,
            handle: Handle::current(),
            bytes: Bytes::new(),
        }
    }
}

impl<S, E> Read for StreamReadable<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    BinstallError: From<E>,
{
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
impl<S, E> BufRead for StreamReadable<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    BinstallError: From<E>,
{
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = &mut self.bytes;

        if !bytes.has_remaining() {
            match self.handle.block_on(async { self.stream.next().await }) {
                Some(Ok(new_bytes)) => *bytes = new_bytes,
                Some(Err(e)) => {
                    let e: BinstallError = e.into();
                    return Err(io::Error::new(io::ErrorKind::Other, e));
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
