use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

#[derive(Debug, Diagnostic, Error)]
pub enum LeonError {
    /// The template failed to parse.
    #[error(transparent)]
    InvalidTemplate(
        #[diagnostic_source]
        #[from]
        ParseError,
    ),

    /// A key was missing from the provided values.
    #[error("missing key `{0}`")]
    MissingKey(String),

    /// An I/O error passed through from [`Template::render_into`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Diagnostic, Error)]
#[error("invalid template")]
pub struct ParseError {
    #[source_code]
    pub(crate) src: String,

    #[label = "unbalanced braces"]
    pub(crate) unbalanced: Option<SourceSpan>,

    #[label = "empty key"]
    pub(crate) empty_key: Option<SourceSpan>,
}
