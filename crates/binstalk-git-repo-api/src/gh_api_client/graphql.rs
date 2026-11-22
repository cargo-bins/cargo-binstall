//! Helpers for writing GraphQL queries.

use std::{fmt, io, str};

/// Lets [`serde_json`], which writes to an [`io::Write`], write into a
/// [`fmt::Write`] such as a `String` or a [`fmt::Formatter`].
struct FmtWriter<W>(W);

impl<W: fmt::Write> io::Write for FmtWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // `serde_json` writes a string as slices of it split at the characters
        // it escapes, which are ASCII, so each slice is valid UTF-8.
        let s =
            str::from_utf8(buf).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        self.0.write_str(s).map_err(io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Displays a `str` as a GraphQL string literal, quotes included, so that a
/// value can't break out of the query.
pub(super) struct GraphQLString<'a>(pub(super) &'a str);

impl fmt::Display for GraphQLString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A JSON string is a valid GraphQL string, with the same escapes.
        serde_json::to_writer(FmtWriter(f), self.0).map_err(|_| fmt::Error)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_graphql_string() {
        assert_eq!(GraphQLString("v0.17.6").to_string(), r#""v0.17.6""#);
        assert_eq!(
            GraphQLString("cargo-audit/\"v0.17.6\"\n").to_string(),
            r#""cargo-audit/\"v0.17.6\"\n""#
        );
        assert_eq!(GraphQLString("é✓").to_string(), r#""é✓""#);
    }
}
