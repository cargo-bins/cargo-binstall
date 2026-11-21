//! A cap on how much an archive may unpack to, against decompression bombs.
//!
//! Extraction runs before the archive's checksum or signature is checked
//! (both are only known once the whole body has been read) and before the
//! user is asked to confirm the install, so an archive that inflates to far
//! more than any real release would fill the disk before anything else gets
//! a chance to reject it.
//!
//! The cap follows the one cargo puts on crate tarballs
//! ([rust-lang/cargo#11337]): an archive may unpack to whichever is larger of
//! a fixed floor and a multiple of its own compressed size. The floor lets
//! small, highly compressible archives through, and the ratio lets
//! legitimately huge ones through, since a release that is gigabytes large
//! compressed may unpack to tens of gigabytes. Only an archive that is both
//! past the floor and inflating well beyond what real data compresses to is
//! stopped.
//!
//! What is counted is what extraction writes to disk, through
//! [`UnpackBudget::writer`] at each place that writes an extracted file, so
//! the cap does not depend on which library decompresses or parses the
//! archive.
//!
//! [rust-lang/cargo#11337]: https://github.com/rust-lang/cargo/pull/11337

use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use super::DownloadError;

/// How much an archive may unpack to: the larger of
/// [`min`](Self::min) bytes and [`ratio`](Self::ratio) times its compressed
/// size.
///
/// What counts as "unpacked" is what is written to disk, and the compressed
/// size is what has been downloaded so far, so the cap grows as the download
/// does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UnpackLimit {
    min: u64,
    ratio: u64,
}

impl UnpackLimit {
    /// The default, the same as cargo uses for crate tarballs: at least
    /// 512 MiB, or 20 times the compressed size.
    pub const DEFAULT: Self = Self::new(512 * 1024 * 1024, 20);

    /// No cap at all.
    pub const UNLIMITED: Self = Self::new(u64::MAX, u64::MAX);

    /// Allow at least `min` bytes, or `ratio` times the compressed size if
    /// that is larger.
    pub const fn new(min: u64, ratio: u64) -> Self {
        Self { min, ratio }
    }

    /// The floor: an archive may always unpack to this much.
    pub const fn min(&self) -> u64 {
        self.min
    }

    /// The multiple of the compressed size an archive may unpack to.
    pub const fn ratio(&self) -> u64 {
        self.ratio
    }

    fn allowed(&self, compressed: u64) -> u64 {
        self.min.max(compressed.saturating_mul(self.ratio))
    }
}

impl Default for UnpackLimit {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Default)]
struct Counters {
    compressed: AtomicU64,
    unpacked: AtomicU64,
}

/// The running tally of one archive's compressed and unpacked bytes, checked
/// against its [`UnpackLimit`].
///
/// Clones share the tally: the download side records what it receives and
/// every file written while extracting draws from the same budget, so the cap
/// applies to the archive as a whole, not to each of its entries.
#[derive(Clone, Debug)]
pub(crate) struct UnpackBudget {
    limit: UnpackLimit,
    counters: Arc<Counters>,
}

impl UnpackBudget {
    pub(crate) fn new(limit: UnpackLimit) -> Self {
        Self {
            limit,
            counters: Arc::default(),
        }
    }

    /// Record `n` more bytes of the archive itself as downloaded.
    pub(crate) fn add_compressed(&self, n: usize) {
        self.counters.compressed.fetch_add(n as u64, Relaxed);
    }

    /// Draw `n` more unpacked bytes, failing once the archive would have
    /// unpacked to more than it is allowed to.
    fn take(&self, n: usize) -> io::Result<()> {
        let unpacked = self
            .counters
            .unpacked
            .fetch_add(n as u64, Relaxed)
            .saturating_add(n as u64);
        let compressed = self.counters.compressed.load(Relaxed);
        let allowed = self.limit.allowed(compressed);

        if unpacked > allowed {
            Err(io::Error::other(DownloadError::UnpackLimitExceeded {
                compressed,
                allowed,
            }))
        } else {
            Ok(())
        }
    }

    /// Wrap the writer of an extracted file so what is written to it draws
    /// from this budget.
    pub(crate) fn writer<W>(&self, inner: W) -> LimitedWriter<W> {
        LimitedWriter {
            inner,
            budget: self.clone(),
        }
    }
}

/// A writer of an extracted file that fails once its archive's
/// [`UnpackBudget`] runs out, before writing the bytes that would go over it.
#[derive(Debug)]
pub(crate) struct LimitedWriter<W> {
    inner: W,
    budget: UnpackBudget,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.budget.take(buf.len())?;
        let n = self.inner.write(buf)?;
        // Give back what the inner writer did not take, so a short write is
        // not counted twice when it is retried.
        self.budget
            .counters
            .unpacked
            .fetch_sub((buf.len() - n) as u64, Relaxed);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exceeded(err: io::Error) -> (u64, u64) {
        match err.downcast::<DownloadError>() {
            Ok(DownloadError::UnpackLimitExceeded {
                compressed,
                allowed,
            }) => (compressed, allowed),
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn allowed_is_the_larger_of_floor_and_ratio() {
        let limit = UnpackLimit::new(100, 20);
        assert_eq!(limit.allowed(0), 100);
        assert_eq!(limit.allowed(5), 100);
        assert_eq!(limit.allowed(6), 120);
        assert_eq!(UnpackLimit::UNLIMITED.allowed(u64::MAX), u64::MAX);
    }

    #[test]
    fn floor_is_always_allowed() {
        let budget = UnpackBudget::new(UnpackLimit::new(100, 20));
        let mut out = Vec::new();
        budget.writer(&mut out).write_all(&[0; 100]).unwrap();
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn going_over_writes_nothing_and_is_shared_between_files() {
        let budget = UnpackBudget::new(UnpackLimit::new(100, 20));
        budget.add_compressed(6);

        let (mut a, mut b) = (Vec::new(), Vec::new());
        budget.writer(&mut a).write_all(&[0; 70]).unwrap();

        // The second file pushes the archive as a whole past 120 bytes.
        let err = budget.writer(&mut b).write_all(&[0; 70]).unwrap_err();
        assert_eq!(exceeded(err), (6, 120));
        assert!(b.is_empty());
    }

    #[test]
    fn cap_grows_with_the_download() {
        let budget = UnpackBudget::new(UnpackLimit::new(100, 20));
        let mut out = Vec::new();
        budget.writer(&mut out).write_all(&[0; 100]).unwrap();
        budget.writer(&mut out).write_all(&[0]).unwrap_err();

        let budget = UnpackBudget::new(UnpackLimit::new(100, 20));
        budget.writer(&mut out).write_all(&[0; 100]).unwrap();
        budget.add_compressed(10);
        budget.writer(&mut out).write_all(&[0; 100]).unwrap();
    }

    #[test]
    fn short_writes_are_counted_once() {
        /// Takes at most one byte per write.
        struct OneByte(Vec<u8>);

        impl Write for OneByte {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.extend(buf.first());
                Ok(buf.len().min(1))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let budget = UnpackBudget::new(UnpackLimit::new(10, 0));
        let mut writer = budget.writer(OneByte(Vec::new()));
        writer.write_all(&[1; 10]).unwrap();
        assert_eq!(writer.inner.0, [1; 10]);
        writer.write_all(&[1]).unwrap_err();
    }
}
