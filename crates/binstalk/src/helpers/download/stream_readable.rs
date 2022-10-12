use std::{
    cmp::min,
    future::Future,
    io::{self, BufRead, Read, Write},
    pin::Pin,
};

use bytes::{Buf, Bytes};
use futures_util::stream::{Stream, StreamExt};
use tokio::runtime::Handle;

use crate::{errors::BinstallError, helpers::signal::wait_on_cancellation_signal};

/// This wraps an AsyncIterator as a `Read`able.
/// It must be used in non-async context only,
/// meaning you have to use it with
/// `tokio::task::{block_in_place, spawn_blocking}` or
/// `std::thread::spawn`.
pub struct StreamReadable<S> {
    stream: S,
    handle: Handle,
    bytes: Bytes,
    cancellation_future: Pin<Box<dyn Future<Output = Result<(), io::Error>> + Send>>,
}

impl<S> StreamReadable<S> {
    pub(super) async fn new(stream: S) -> Self {
        Self {
            stream,
            handle: Handle::current(),
            bytes: Bytes::new(),
            cancellation_future: Box::pin(wait_on_cancellation_signal()),
        }
    }
}

impl<S, E> StreamReadable<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    BinstallError: From<E>,
{
    /// Copies from `self` to `writer`.
    ///
    /// Same as `io::copy` but does not allocate any internal buffer
    /// since `self` is buffered.
    pub(super) fn copy<W>(&mut self, mut writer: W) -> io::Result<()>
    where
        W: Write,
    {
        self.copy_inner(&mut writer)
    }

    fn copy_inner(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        loop {
            let buf = self.fill_buf()?;
            if buf.is_empty() {
                // Eof
                break Ok(());
            }

            writer.write_all(buf)?;

            let n = buf.len();
            self.consume(n);
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

async fn next_stream<S, E>(stream: &mut S) -> io::Result<Option<Bytes>>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    BinstallError: From<E>,
{
    stream
        .next()
        .await
        .transpose()
        .map_err(BinstallError::from)
        .map_err(io::Error::from)
}

impl<S, E> BufRead for StreamReadable<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    BinstallError: From<E>,
{
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = &mut self.bytes;

        if !bytes.has_remaining() {
            let option = self.handle.block_on(async {
                tokio::select! {
                    res = next_stream(&mut self.stream) => res,
                    res = self.cancellation_future.as_mut() => {
                        Err(res.err().unwrap_or_else(|| io::Error::from(BinstallError::UserAbort)))
                    },
                }
            })?;

            if let Some(new_bytes) = option {
                *bytes = new_bytes;
            }
        }
        Ok(&*bytes)
    }

    fn consume(&mut self, amt: usize) {
        self.bytes.advance(amt);
    }
}
