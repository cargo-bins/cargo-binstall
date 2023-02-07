use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

#[derive(Debug, Diagnostic, Error)]
pub enum LeonError<'s> {
    /// The template failed to parse.
    #[error(transparent)]
    InvalidTemplate(
        #[diagnostic_source]
        #[from]
        ParseError<'static>,
        // 'static is required due to limitations of the std::Error trait
    ),

    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(&'s str),

    /// An I/O error passed through from [`Template::render_into`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Diagnostic, Error)]
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
