//! Error handling shared by binstalk crates, built on [`exn`].
//!
//! Most errors are never inspected, only reported, so functions return the
//! type-erased [`Result`] and `?` converts any [`std::error::Error`] or
//! [`Exn<E>`] into it while recording the caller location.
//!
//! Context is added with `.or_raise(|| message!(...))`; errors that callers
//! do recover from keep their own type and are looked up with
//! [`FrameExt::find`].

use std::{error::Error as StdError, fmt, io};

use compact_str::{CompactString, ToCompactString};

pub use exn::{self, bail, ensure, ErrorExt, Exn, Frame, IteratorExt};

/// A type-erased tree of errors.
pub type Error = Exn;

/// `Result` with a type-erased [`Error`] by default.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Add context to the error of a [`Result`], returning the type-erased [`Result`].
///
/// Unlike [`exn::ResultExt`], the result is always type-erased, so it can be
/// returned directly from a function returning [`Result`].
pub trait ResultExt<T> {
    /// Raise the error of `self` as a child of `err()`.
    ///
    /// `err` is only called on error, use it when creating the parent error
    /// costs something, e.g. a [`message!`](crate::message) with arguments.
    fn or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A;

    /// Raise the error of `self` as a child of `err`.
    ///
    /// Use it when `err` is free to create, e.g. an `Exit` code or a
    /// [`message!`](crate::message) of a string literal.
    fn or_raise_err<A>(self, err: A) -> Result<T>
    where
        A: StdError + Send + Sync + 'static;
}

impl<T, E> ResultExt<T> for std::result::Result<T, E>
where
    Error: From<E>,
{
    #[track_caller]
    fn or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A,
    {
        match self {
            Ok(v) => Ok(v),
            // `Error::from` (not `.into()`) so that the location of the
            // child is the caller's too.
            Err(e) => Err(Error::from(e).raise(err()).into()),
        }
    }

    #[track_caller]
    fn or_raise_err<A>(self, err: A) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
    {
        match self {
            Ok(v) => Ok(v),
            Err(e) => Err(Error::from(e).raise(err).into()),
        }
    }
}

/// Turn `None` into an error, returning the type-erased [`Result`].
pub trait OptionExt<T> {
    fn ok_or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A;
}

impl<T> OptionExt<T> for Option<T> {
    #[track_caller]
    fn ok_or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A,
    {
        match self {
            Some(v) => Ok(v),
            None => Err(Exn::from(err())),
        }
    }
}

/// A plain-text error, used to add context to an error tree.
///
/// Create it with [`message!`](crate::message).
#[derive(Clone, Debug)]
pub struct Message {
    message: CompactString,
    help: Option<CompactString>,
}

impl Message {
    pub fn new(message: impl Into<CompactString>) -> Self {
        Self {
            message: message.into(),
            help: None,
        }
    }

    /// Create from a `&'static str`, usable in `const`.
    pub const fn from_static(message: &'static str) -> Self {
        Self {
            message: CompactString::const_new(message),
            help: None,
        }
    }

    /// Create from [`format_args!`], without allocation for a string literal.
    pub fn from_args(args: fmt::Arguments<'_>) -> Self {
        Self::new(match args.as_str() {
            Some(s) => CompactString::const_new(s),
            None => args.to_compact_string(),
        })
    }

    /// Attach a hint on how to fix the error, shown as `help:` when rendered.
    pub fn with_help(mut self, help: impl Into<CompactString>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn help(&self) -> Option<&str> {
        self.help.as_deref()
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for Message {}

#[doc(hidden)]
pub const fn __has_brace(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            return true;
        }
        i += 1;
    }
    false
}

/// Create a [`Message`] with the syntax of [`format!`].
#[macro_export]
macro_rules! message {
    // A literal without `{` has nothing to format, so the `Message` is built
    // at compile time. With `{` it may capture variables, e.g. `"{path}"`.
    ($msg:literal $(,)?) => {{
        if const { $crate::errors::__has_brace($msg) } {
            $crate::errors::Message::from_args(::std::format_args!($msg))
        } else {
            const { $crate::errors::Message::from_static($msg) }
        }
    }};
    ($($arg:tt)*) => {
        $crate::errors::Message::from_args(::std::format_args!($($arg)*))
    };
}

/// Walk the tree of an error, implemented for [`Frame`] and [`Exn`].
pub trait FrameExt {
    /// The root frame of the tree.
    fn as_frame(&self) -> &Frame;

    /// Find the first error of type `T` in the tree, depth-first.
    fn find<T: StdError + 'static>(&self) -> Option<&T> {
        self.find_map(Some)
    }

    /// Find the first error of type `T` in the tree (depth-first, pre-order)
    /// for which `f` returns `Some`.
    fn find_map<'a, T, U>(&'a self, mut f: impl FnMut(&'a T) -> Option<U>) -> Option<U>
    where
        T: StdError + 'static,
    {
        fn inner<'a, T: StdError + 'static, U>(
            frame: &'a Frame,
            f: &mut dyn FnMut(&'a T) -> Option<U>,
        ) -> Option<U> {
            frame
                .error()
                .downcast_ref::<T>()
                .and_then(&mut *f)
                .or_else(|| frame.children().iter().find_map(|child| inner(child, f)))
        }

        inner(self.as_frame(), &mut f)
    }

    /// Display the error and its causes on one line, e.g. `a: b: c`, or
    /// `a: [b; c]` if `a` has multiple children.
    ///
    /// Unlike the `Debug` output of [`Exn`], it contains no source locations.
    fn display_chain(&self) -> impl fmt::Display + '_ {
        DisplayChain(self.as_frame())
    }
}

impl FrameExt for Frame {
    fn as_frame(&self) -> &Frame {
        self
    }
}

impl<E: StdError + Send + Sync + 'static + ?Sized> FrameExt for Exn<E> {
    fn as_frame(&self) -> &Frame {
        self.frame()
    }
}

struct DisplayChain<'a>(&'a Frame);

impl fmt::Display for DisplayChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0.error(), f)?;
        match self.0.children() {
            [] => Ok(()),
            [child] => write!(f, ": {}", DisplayChain(child)),
            children => {
                f.write_str(": [")?;
                for (i, child) in children.iter().enumerate() {
                    if i != 0 {
                        f.write_str("; ")?;
                    }
                    fmt::Display::fmt(&DisplayChain(child), f)?;
                }
                f.write_str("]")
            }
        }
    }
}

/// Carries an [`Error`] through an [`io::Error`], e.g. through an
/// [`AsyncRead`](https://docs.rs/tokio/latest/tokio/io/trait.AsyncRead.html)
/// adapter, so that [`from_io`] can recover the whole tree.
struct IoCarrier(Error);

impl fmt::Debug for IoCarrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for IoCarrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl StdError for IoCarrier {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0.frame().source()
    }
}

/// Wrap `err` in an [`io::Error`], to be recovered by [`from_io`].
pub fn into_io(err: impl Into<Error>) -> io::Error {
    io::Error::other(IoCarrier(err.into()))
}

/// Recover the [`Error`] wrapped by [`into_io`], or wrap `err` itself.
#[track_caller]
pub fn from_io(err: io::Error) -> Error {
    match err.downcast::<IoCarrier>() {
        Ok(carrier) => carrier.0,
        Err(err) => Exn::from(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Kind;

    impl fmt::Display for Kind {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("kind")
        }
    }

    impl StdError for Kind {}

    #[test]
    fn display_chain_one_line() {
        let err: Error = io::Error::other("c")
            .raise()
            .raise(Message::new("b"))
            .raise(Message::new("a"))
            .into();
        assert_eq!(err.display_chain().to_string(), "a: b: c");
    }

    #[test]
    fn find_and_io_round_trip() {
        let err: Error = Kind.raise().raise(crate::message!("context {}", 1)).into();
        assert!(from_io(into_io(err)).find::<Kind>().is_some());

        // A literal with `{` may capture variables.
        let path = "some/path";
        assert_eq!(crate::message!("{path}").to_string(), "some/path");
        assert_eq!(crate::message!("{{}}").to_string(), "{}");

        // A long string literal is borrowed, a short one is inlined, neither allocates.
        let msg = crate::message!("a string literal longer than 24 bytes");
        assert_eq!(
            msg.message.as_static_str(),
            Some("a string literal longer than 24 bytes")
        );
    }
}
