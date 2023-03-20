#[cfg(feature = "miette")]
use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

#[cfg_attr(feature = "miette", derive(Diagnostic))]
#[derive(Debug, Error)]
pub enum RenderError {
    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(String),

    /// An I/O error passed through from [`Template::render_into`].
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// An error that can occur when parsing a template.
///
/// When the `miette` feature is enabled, this is a rich miette-powered error
/// which will highlight the source of the error in the template when output
/// (with miette's `fancy` feature). With `miette` disabled, this is opaque.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[cfg_attr(feature = "miette", derive(Diagnostic))]
#[cfg_attr(feature = "miette", diagnostic(transparent))]
#[error(transparent)]
pub struct ParseError(Box<InnerParseError>);

/// The inner (unboxed) type of [`ParseError`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[cfg_attr(feature = "miette", derive(Diagnostic))]
#[error("template parse failed")]
struct InnerParseError {
    #[cfg_attr(feature = "miette", source_code)]
    src: String,

    #[cfg_attr(feature = "miette", label("This bracket is not opening or closing anything. Try removing it, or escaping it with a backslash."))]
    unbalanced: Option<SourceSpan>,

    #[cfg_attr(feature = "miette", label("This escape is malformed."))]
    escape: Option<SourceSpan>,

    #[cfg_attr(feature = "miette", label("A key cannot be empty."))]
    key_empty: Option<SourceSpan>,

    #[cfg_attr(feature = "miette", label("Escapes are not allowed in keys."))]
    key_escape: Option<SourceSpan>,
}

impl ParseError {
    pub(crate) fn unbalanced(src: &str, start: usize, end: usize) -> Self {
        Self(Box::new(InnerParseError {
            src: String::from(src),
            unbalanced: Some((start, end.saturating_sub(start) + 1).into()),
            escape: None,
            key_empty: None,
            key_escape: None,
        }))
    }

    pub(crate) fn escape(src: &str, start: usize, end: usize) -> Self {
        Self(Box::new(InnerParseError {
            src: String::from(src),
            unbalanced: None,
            escape: Some((start, end.saturating_sub(start) + 1).into()),
            key_empty: None,
            key_escape: None,
        }))
    }

    pub(crate) fn key_empty(src: &str, start: usize, end: usize) -> Self {
        Self(Box::new(InnerParseError {
            src: String::from(src),
            unbalanced: None,
            escape: None,
            key_empty: Some((start, end.saturating_sub(start) + 1).into()),
            key_escape: None,
        }))
    }

    pub(crate) fn key_escape(src: &str, start: usize, end: usize) -> Self {
        Self(Box::new(InnerParseError {
            src: String::from(src),
            unbalanced: None,
            escape: None,
            key_empty: None,
            key_escape: Some((start, end.saturating_sub(start) + 1).into()),
        }))
    }
}
