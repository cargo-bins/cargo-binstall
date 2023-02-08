use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

#[derive(Debug, Diagnostic, Error)]
pub enum RenderError<'s> {
    /// The template failed to parse.
    #[error(transparent)]
    InvalidTemplate(
        #[diagnostic_source]
        #[from]
        BoxedParseError,
    ),

    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(&'s str),

    /// An I/O error passed through from [`Template::render_into`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct BoxedParseError(#[from] pub Box<ParseError<'static>>);

impl Diagnostic for BoxedParseError {
    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        Some(&*self.0)
    }
}

#[derive(Debug, Diagnostic, Error, PartialEq, Eq)]
#[error("invalid template")]
pub struct ParseError<'s> {
    #[source_code]
    pub(crate) src: &'s str,

    #[label = "these braces are unbalanced"]
    pub(crate) unbalanced: Option<SourceSpan>,

    #[label = "this escape is malformed"]
    pub(crate) escape: Option<SourceSpan>,

    #[label = "a key cannot be empty"]
    pub(crate) key_empty: Option<SourceSpan>,

    #[label = "escapes are not allowed in keys"]
    pub(crate) key_escape: Option<SourceSpan>,
}
