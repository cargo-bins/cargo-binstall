//! Helpers for writing GraphQL queries.

use std::fmt::{self, Write};

/// Displays a `str` as a GraphQL string literal, quotes included, so that a
/// value can't break out of the query.
pub(super) struct GraphQLString<'a>(pub(super) &'a str);

impl fmt::Display for GraphQLString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_char('"')?;

        // Write everything up to the next character that must be escaped as
        // is, all of which are ASCII.
        let mut rest = self.0;
        while let Some(i) = rest.find(|c: char| matches!(c, '"' | '\\') || c.is_ascii_control()) {
            f.write_str(&rest[..i])?;
            match rest.as_bytes()[i] {
                b'"' => f.write_str(r#"\""#)?,
                b'\\' => f.write_str(r"\\")?,
                control => write!(f, r"\u{control:04x}")?,
            }
            rest = &rest[i + 1..];
        }
        f.write_str(rest)?;

        f.write_char('"')
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_graphql_string() {
        assert_eq!(GraphQLString("").to_string(), r#""""#);
        assert_eq!(GraphQLString("v0.17.6").to_string(), r#""v0.17.6""#);
        assert_eq!(
            GraphQLString("cargo-audit/\"v0.17.6\"\n\\").to_string(),
            r#""cargo-audit/\"v0.17.6\"\u000a\\""#
        );
        assert_eq!(GraphQLString("é✓").to_string(), r#""é✓""#);
    }
}
