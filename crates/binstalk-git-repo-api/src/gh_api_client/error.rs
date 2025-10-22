use std::{error, fmt, io, time::Duration};

use binstalk_downloader::remote;
use compact_str::{CompactString, ToCompactString};
use serde::{de::Deserializer, Deserialize};
use thiserror::Error as ThisError;

#[derive(ThisError, Debug)]
#[error("Context: '{context}', err: '{err}'")]
pub struct GhApiContextError {
    context: CompactString,
    #[source]
    err: GhApiError,
}

#[derive(ThisError, Debug)]
#[non_exhaustive]
pub enum GhApiError {
    #[error("IO Error: {0}")]
    Io(#[from] io::Error),

    #[error("Remote Error: {0}")]
    Remote(#[from] remote::Error),

    #[error("Failed to parse url: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// A wrapped error providing the context the error is about.
    #[error(transparent)]
    Context(Box<GhApiContextError>),

    #[error("Remote failed to process GraphQL query: {0}")]
    GraphQLErrors(GhGraphQLErrors),

    #[error("Hit rate-limit, retry after {retry_after:?}")]
    RateLimit { retry_after: Option<Duration> },

    #[error("Corresponding resource is not found")]
    NotFound,

    #[error("Does not have permission to access the API")]
    Unauthorized,
}

impl GhApiError {
    /// Attach context to [`GhApiError`]
    pub fn context(self, context: impl fmt::Display) -> Self {
        use GhApiError::*;

        if matches!(self, RateLimit { .. } | NotFound | Unauthorized) {
            self
        } else {
            Self::Context(Box::new(GhApiContextError {
                context: context.to_compact_string(),
                err: self,
            }))
        }
    }
}

impl From<GhGraphQLErrors> for GhApiError {
    fn from(e: GhGraphQLErrors) -> Self {
        if e.is_rate_limited() {
            Self::RateLimit { retry_after: None }
        } else if e.is_not_found_error() {
            Self::NotFound
        } else {
            Self::GraphQLErrors(e)
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct GhGraphQLErrors(Box<[GraphQLError]>);

impl GhGraphQLErrors {
    fn is_rate_limited(&self) -> bool {
        self.0
            .iter()
            .any(|error| matches!(error.error_type, GraphQLErrorType::RateLimited))
    }

    fn is_not_found_error(&self) -> bool {
        self.0
            .iter()
            .any(|error| matches!(&error.error_type, GraphQLErrorType::Other(error_type) if *error_type == "NOT_FOUND"))
    }
}

impl error::Error for GhGraphQLErrors {}

impl fmt::Display for GhGraphQLErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last_error_index = self.0.len() - 1;

        for (i, error) in self.0.iter().enumerate() {
            write!(
                f,
                "type: '{error_type}', msg: '{msg}'",
                error_type = error.error_type,
                msg = error.message,
            )?;

            for location in error.locations.as_deref().into_iter().flatten() {
                write!(
                    f,
                    ", occurred on query line {line} col {col}",
                    line = location.line,
                    col = location.column
                )?;
            }

            for (k, v) in &error.others {
                write!(f, ", {k}: {v}")?;
            }

            if i < last_error_index {
                f.write_str("\n")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: CompactString,
    locations: Option<Box<[GraphQLLocation]>>,

    #[serde(rename = "type")]
    error_type: GraphQLErrorType,

    #[serde(flatten, with = "tuple_vec_map")]
    others: Vec<(CompactString, serde_json::Value)>,
}

#[derive(Debug)]
pub(super) enum GraphQLErrorType {
    RateLimited,
    Other(CompactString),
}

impl fmt::Display for GraphQLErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GraphQLErrorType::RateLimited => "RATE_LIMITED",
            GraphQLErrorType::Other(s) => s,
        })
    }
}

impl<'de> Deserialize<'de> for GraphQLErrorType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = CompactString::deserialize(deserializer)?;
        Ok(match &*s {
            "RATE_LIMITED" => GraphQLErrorType::RateLimited,
            _ => GraphQLErrorType::Other(s),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLLocation {
    line: u64,
    column: u64,
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::de::value::{BorrowedStrDeserializer, Error};

    macro_rules! assert_matches {
        ($expression:expr, $pattern:pat $(if $guard:expr)? $(,)?) => {
            match $expression {
                $pattern $(if $guard)? => true,
                expr => {
                    panic!(
                        "assertion failed: `{expr:?}` does not match `{}`",
                        stringify!($pattern $(if $guard)?)
                    )
                }
            }
        }
    }

    #[test]
    fn test_graph_ql_error_type() {
        let deserialize = |input: &str| {
            GraphQLErrorType::deserialize(BorrowedStrDeserializer::<'_, Error>::new(input)).unwrap()
        };

        assert_matches!(deserialize("RATE_LIMITED"), GraphQLErrorType::RateLimited);
        assert_matches!(
            deserialize("rATE_LIMITED"),
            GraphQLErrorType::Other(val) if val == CompactString::const_new("rATE_LIMITED")
        );
    }
}
