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
/// This is a rich miette-powered error which will highlight the source of the
/// error in the template when output (with miette's `fancy` feature). If you
/// don't want or need that, you can disable the `miette` feature and a simpler
/// opaque error will be substituted.
#[cfg(feature = "miette")]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(transparent)]
pub struct ParseError(pub Box<InnerParseError>);

#[cfg(feature = "miette")]
impl Diagnostic for ParseError {
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.0.source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.0.labels()
    }
}

/// The inner (unboxed) type of [`ParseError`].
#[cfg(feature = "miette")]
#[derive(Clone, Debug, Diagnostic, Error, PartialEq, Eq)]
#[error("template parse failed")]
#[non_exhaustive]
pub struct InnerParseError {
    #[source_code]
    pub src: String,

    #[label("This bracket is not opening or closing anything. Try removing it, or escaping it with a backslash.")]
    pub unbalanced: Option<SourceSpan>,

    #[label("This escape is malformed.")]
    pub escape: Option<SourceSpan>,

    #[label("A key cannot be empty.")]
    pub key_empty: Option<SourceSpan>,

    #[label("Escapes are not allowed in keys.")]
    pub key_escape: Option<SourceSpan>,
}

#[cfg(feature = "miette")]
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

/// An opaque parsing error.
///
/// This is the non-miette version of this error, which only implements
/// [`std::error::Error`] and [`std::fmt::Display`], and does not provide any
/// programmably-useable detail besides a message.
#[cfg(not(feature = "miette"))]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("template parse failed: {0}")]
pub struct ParseError(String);

#[cfg(not(feature = "miette"))]
impl ParseError {
    pub(crate) fn unbalanced(src: &str, start: usize, end: usize) -> Self {
        Self(format!("unbalanced brace at {start}:{end} in {src:?}"))
    }

    pub(crate) fn escape(src: &str, start: usize, end: usize) -> Self {
        Self(format!("malformed escape at {start}:{end} in {src:?}"))
    }

    pub(crate) fn key_empty(src: &str, start: usize, end: usize) -> Self {
        Self(format!("empty key at {start}:{end} in {src:?}"))
    }

    pub(crate) fn key_escape(src: &str, start: usize, end: usize) -> Self {
        Self(format!("escape in key at {start}:{end} in {src:?}"))
    }
}
