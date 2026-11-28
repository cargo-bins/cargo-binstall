use std::{
    fmt,
    ops::{Deref, DerefMut, Range},
};

use zeroize::Zeroizing;

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl Redacted<Zeroizing<String>> {
    pub fn from_string(value: String) -> Self {
        Self::new(Zeroizing::new(value))
    }

    pub fn from_boxed_str(value: Box<str>) -> Self {
        Self::from_string(value.into())
    }
}

impl<T> Deref for Redacted<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for Redacted<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

pub type SecretString = Redacted<Zeroizing<String>>;

/// `url` with the user name and password it may hold replaced, for logs and
/// error messages.
///
/// A token is commonly passed as either of them, e.g.
/// `https://<token>@github.com/owner/repo` or
/// `sparse+https://user:<token>@registry.example.com/index/`.
///
/// Takes a string (a `&str`, `Url::as_str`, `Cow<str>`, ...) and writes the
/// parts around them, without allocating.
pub fn redact_url_userinfo<S: AsRef<str>>(url: S) -> RedactUrlUserinfo<S> {
    RedactUrlUserinfo(url)
}

/// See [`redact_url_userinfo`].
#[derive(Clone, Copy)]
pub struct RedactUrlUserinfo<S>(S);

impl<S: AsRef<str>> fmt::Display for RedactUrlUserinfo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.0.as_ref();
        match userinfo_range(url) {
            Some(userinfo) => {
                f.write_str(&url[..userinfo.start])?;
                f.write_str("<redacted>")?;
                f.write_str(&url[userinfo.end..])
            }
            None => f.write_str(url),
        }
    }
}

/// Where the user name and password are in `url`: from after the first `://`
/// to the last `@` before the authority ends at a `/`, `?` or `#`.
fn userinfo_range(url: &str) -> Option<Range<usize>> {
    let start = url.find("://")? + "://".len();
    let rest = &url[start..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let at = rest[..authority_len].rfind('@')?;
    Some(start..start + at)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_redact_url_userinfo() {
        for (url, expected) in [
            (
                "https://ghp_token@github.com/owner/repo",
                "https://<redacted>@github.com/owner/repo",
            ),
            (
                "sparse+https://user:p%40ss@registry.example.com:8080/index/?a=b",
                "sparse+https://<redacted>@registry.example.com:8080/index/?a=b",
            ),
            ("https://user:pa@ss@host", "https://<redacted>@host"),
            (
                "--index=https://token@host/",
                "--index=https://<redacted>@host/",
            ),
        ] {
            assert_eq!(redact_url_userinfo(url).to_string(), expected);
        }

        // Nothing to redact.
        for url in [
            "https://github.com/owner/repo",
            "https://example.com/a@b?c=d@e#f@g",
            "git@github.com:owner/repo.git",
            "cargo-binstall",
            "",
        ] {
            assert_eq!(redact_url_userinfo(url).to_string(), url);
        }
    }
}
