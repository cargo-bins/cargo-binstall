//! Error handling shared by binstalk crates, built on [`exn`].
//!
//! Most errors are never inspected, only reported, so functions return the
//! type-erased [`Result`] and `?` converts any [`std::error::Error`] or
//! [`Exn<E>`] into it while recording the caller location.
//!
//! Context is added with `.or_raise(|| message!(...))`; errors that callers
//! do recover from keep their own type and are looked up with [`find`].

use std::{borrow::Cow, error::Error as StdError, fmt, io};

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
    fn or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A;
}

impl<T, E: Into<Error>> ResultExt<T> for std::result::Result<T, E> {
    #[track_caller]
    fn or_raise<A, F>(self, err: F) -> Result<T>
    where
        A: StdError + Send + Sync + 'static,
        F: FnOnce() -> A,
    {
        match self {
            Ok(v) => Ok(v),
            Err(e) => Err(e.into().raise(err()).into()),
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
    message: Cow<'static, str>,
    help: Option<Cow<'static, str>>,
}

impl Message {
    pub fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            message: message.into(),
            help: None,
        }
    }

    /// Create from [`format_args!`], without allocation for a string literal.
    pub fn from_args(args: fmt::Arguments<'_>) -> Self {
        Self::new(match args.as_str() {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(args.to_string()),
        })
    }

    /// Attach a hint on how to fix the error, shown as `help:` when rendered.
    pub fn with_help(mut self, help: impl Into<Cow<'static, str>>) -> Self {
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

/// Create a [`Message`] with the syntax of [`format!`].
#[macro_export]
macro_rules! message {
    ($($arg:tt)*) => {
        $crate::errors::Message::from_args(::std::format_args!($($arg)*))
    };
}

/// Display the error of `frame` and its causes on one line, e.g.
/// `a: b: c`, or `a: [b; c]` if `a` has multiple children.
///
/// Unlike the `Debug` output of [`Exn`], it contains no source locations.
pub fn display_chain(frame: &Frame) -> impl fmt::Display + '_ {
    struct Chain<'a>(&'a Frame);

    impl fmt::Display for Chain<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Display::fmt(self.0.error(), f)?;
            match self.0.children() {
                [] => Ok(()),
                [child] => write!(f, ": {}", Chain(child)),
                children => {
                    f.write_str(": [")?;
                    for (i, child) in children.iter().enumerate() {
                        if i != 0 {
                            f.write_str("; ")?;
                        }
                        fmt::Display::fmt(&Chain(child), f)?;
                    }
                    f.write_str("]")
                }
            }
        }
    }

    Chain(frame)
}

/// Find the first error of type `T` in the tree of `frame`, depth-first.
pub fn find<T: StdError + 'static>(frame: &Frame) -> Option<&T> {
    frame
        .error()
        .downcast_ref::<T>()
        .or_else(|| frame.children().iter().find_map(find::<T>))
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
        assert_eq!(display_chain(err.frame()).to_string(), "a: b: c");
    }

    #[test]
    fn find_and_io_round_trip() {
        let err: Error = Kind.raise().raise(crate::message!("context {}", 1)).into();
        assert!(find::<Kind>(from_io(into_io(err)).frame()).is_some());

        let msg = crate::message!("literal");
        assert!(matches!(msg.message, Cow::Borrowed("literal")));
    }
}
