use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
#[cfg_attr(feature = "miette", derive(miette::Diagnostic))]
pub enum RenderError {
    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(String),

    /// An I/O error passed through from [`Template::render_into`](crate::Template::render_into).
    #[error("write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// An error that can occur when parsing a template.
///
/// When the `miette` feature is enabled, this is a rich miette-powered error
/// which will highlight the source of the error in the template when output
/// (with miette's `fancy` feature). With `miette` disabled, this is opaque.
#[derive(Clone, Debug, ThisError, PartialEq, Eq)]
#[cfg_attr(feature = "miette", derive(miette::Diagnostic))]
#[cfg_attr(feature = "miette", diagnostic(transparent))]
#[error(transparent)]
pub struct ParseError(Box<InnerParseError>);

/// The inner (unboxed) type of [`ParseError`].
#[derive(Clone, Debug, ThisError, PartialEq, Eq)]
#[error("{kind} at span start = {offset}, len = {len}: {src}")]
struct InnerParseError {
    src: String,
    offset: usize,
    len: usize,
    kind: ErrorKind,
}

#[cfg(feature = "miette")]
impl miette::Diagnostic for InnerParseError {
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.src)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(std::iter::once_with(|| {
            miette::LabeledSpan::new(Some(self.kind.to_string()), self.offset, self.len)
        })))
    }
}

#[derive(Clone, Debug, ThisError, PartialEq, Eq)]
enum ErrorKind {
    #[error("This bracket is not opening or closing anything. Try removing it, or escaping it with a backslash.")]
    Unbalanced,

    #[error("This escape is malformed.")]
    Escape,

    #[error("A key cannot be empty.")]
    KeyEmpty,

    #[error("Escapes are not allowed in keys.")]
    KeyEscape,
}

impl ParseError {
    fn new(src: &str, start: usize, end: usize, kind: ErrorKind) -> Self {
        Self(Box::new(InnerParseError {
            src: String::from(src),
            offset: start,
            len: end.saturating_sub(start) + 1,
            kind,
        }))
    }

    pub(crate) fn unbalanced(src: &str, start: usize, end: usize) -> Self {
        Self::new(src, start, end, ErrorKind::Unbalanced)
    }

    pub(crate) fn escape(src: &str, start: usize, end: usize) -> Self {
        Self::new(src, start, end, ErrorKind::Escape)
    }

    pub(crate) fn key_empty(src: &str, start: usize, end: usize) -> Self {
        Self::new(src, start, end, ErrorKind::KeyEmpty)
    }

    pub(crate) fn key_escape(src: &str, start: usize, end: usize) -> Self {
        Self::new(src, start, end, ErrorKind::KeyEscape)
    }
}
