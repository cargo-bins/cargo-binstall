use std::{error, fmt, time::Duration};

#[allow(unused_imports)] // used in docs
use binstalk_downloader::remote;
use compact_str::{CompactString, ToCompactString};
use exn::{ErrorExt, Exn};
use serde::{de::Deserializer, Deserialize};
use thiserror::Error as ThisError;

/// Error returned by [`GhApiClient`](super::GhApiClient).
///
/// The underlying cause (e.g. [`remote::Error`] or [`GhGraphQLErrors`]) is
/// attached as a child frame of the [`Exn`].
///
/// [`GhApiError::RateLimit`], [`GhApiError::NotFound`] and
/// [`GhApiError::Unauthorized`] are always kept at the root of the [`Exn`],
/// so callers can match on them via `Deref`.
#[derive(ThisError, Debug)]
#[non_exhaustive]
pub enum GhApiError {
    #[error("IO Error")]
    Io,

    #[error("Remote Error")]
    Remote,

    #[error("Failed to parse url")]
    InvalidUrl,

    /// Context the error is about, e.g. which API was used.
    #[error("Context: '{0}'")]
    Context(CompactString),

    #[error("Remote failed to process GraphQL query")]
    GraphQLErrors,

    #[error("Hit rate-limit, retry after {retry_after:?}")]
    RateLimit { retry_after: Option<Duration> },

    #[error("Corresponding resource is not found")]
    NotFound,

    #[error("Does not have permission to access the API")]
    Unauthorized,
}

impl GhApiError {
    /// Returns `Some(retry_after)` if this is [`GhApiError::RateLimit`].
    pub fn rate_limit(&self) -> Option<Option<Duration>> {
        match self {
            Self::RateLimit { retry_after } => Some(*retry_after),
            _ => None,
        }
    }

    /// Returns true if this is [`GhApiError::NotFound`].
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }

    /// Returns true if this is [`GhApiError::Unauthorized`].
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }

    /// Attach context to `err`, unless it is one of the variants that callers
    /// use for recovery, which must stay at the root of the [`Exn`].
    #[track_caller]
    pub fn context(err: Exn<Self>, context: impl fmt::Display) -> Exn<Self> {
        use GhApiError::*;

        if matches!(&*err, RateLimit { .. } | NotFound | Unauthorized) {
            err
        } else {
            err.raise(Context(context.to_compact_string()))
        }
    }
}

impl GhGraphQLErrors {
    /// Classify the errors into [`GhApiError`], keeping `self` as the cause
    /// unless it is a rate-limit or not-found error.
    #[track_caller]
    pub(super) fn classify(self) -> Exn<GhApiError> {
        if self.is_rate_limited() {
            GhApiError::RateLimit { retry_after: None }.raise()
        } else if self.is_not_found_error() {
            GhApiError::NotFound.raise()
        } else {
            self.raise().raise(GhApiError::GraphQLErrors)
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
