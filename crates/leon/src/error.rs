#[cfg(feature = "miette")]
use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

#[cfg_attr(feature = "miette", derive(Diagnostic))]
#[derive(Debug, Error)]
pub enum RenderError<'s> {
    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(&'s str),

    /// An I/O error passed through from [`Template::render_into`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(feature = "miette")]
#[derive(Debug, Diagnostic, Error, PartialEq, Eq)]
#[error("invalid template")]
#[non_exhaustive]
pub struct ParseError<'s> {
    #[source_code]
    pub src: &'s str,

    #[label = "these braces are unbalanced"]
    pub unbalanced: Option<SourceSpan>,

    #[label = "this escape is malformed"]
    pub escape: Option<SourceSpan>,

    #[label = "a key cannot be empty"]
    pub key_empty: Option<SourceSpan>,

    #[label = "escapes are not allowed in keys"]
    pub key_escape: Option<SourceSpan>,
}

#[cfg(feature = "miette")]
impl<'s> ParseError<'s> {
    pub(crate) fn unbalanced(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            unbalanced: Some((start, end).into()),
            escape: None,
            key_empty: None,
            key_escape: None,
        }
    }

    pub(crate) fn escape(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            unbalanced: None,
            escape: Some((start, end).into()),
            key_empty: None,
            key_escape: None,
        }
    }

    pub(crate) fn key_empty(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            unbalanced: None,
            escape: None,
            key_empty: Some((start, end).into()),
            key_escape: None,
        }
    }

    pub(crate) fn key_escape(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            unbalanced: None,
            escape: None,
            key_empty: None,
            key_escape: Some((start, end).into()),
        }
    }
}

/// An opaque parsing error.
///
/// This is the non-miette version of this error, which only implements
/// [`std::error::Error`] and [`std::fmt::Display`], and does not provide any
/// programmably-useable detail besides a message.
#[cfg(not(feature = "miette"))]
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid template: {problem} at range {start}:{end} in {src:?}")]
pub struct ParseError<'s> {
    src: &'s str,
    problem: &'static str,
    start: usize,
    end: usize,
}

#[cfg(not(feature = "miette"))]
impl<'s> ParseError<'s> {
    pub(crate) fn unbalanced(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            problem: "unbalanced braces",
            start,
            end,
        }
    }

    pub(crate) fn escape(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            problem: "malformed escape",
            start,
            end,
        }
    }

    pub(crate) fn key_empty(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            problem: "empty key",
            start,
            end,
        }
    }

    pub(crate) fn key_escape(src: &'s str, start: usize, end: usize) -> Self {
        Self {
            src,
            problem: "escape in key",
            start,
            end,
        }
    }
}
