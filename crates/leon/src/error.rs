use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

use crate::Literal;

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
    MissingKey(Literal<'s>),

    /// An I/O error passed through from [`Template::render_into`].
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Diagnostic, Error)]
#[error("invalid template")]
pub struct ParseError<'s> {
    #[source_code]
    pub(crate) src: Literal<'s>,

    #[label = "unbalanced braces"]
    pub(crate) unbalanced: Option<SourceSpan>,

    #[label = "empty key"]
    pub(crate) empty_key: Option<SourceSpan>,
}
