use std::{
    fmt,
    ops::{Deref, DerefMut},
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
/// parts around them, without allocating. For a URL that is only
/// `Display`, see [`redact_displayed_url_userinfo`].
pub fn redact_url_userinfo<S: AsRef<str>>(url: S) -> RedactUrlUserinfo<S> {
    RedactUrlUserinfo(url)
}

/// See [`redact_url_userinfo`].
#[derive(Clone, Copy)]
pub struct RedactUrlUserinfo<S>(S);

impl<S: AsRef<str>> fmt::Display for RedactUrlUserinfo<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.0.as_ref();
        match UserinfoFinder::find(url) {
            Some(userinfo) => {
                f.write_str(&url[..userinfo.start])?;
                f.write_str("<redacted>")?;
                f.write_str(&url[userinfo.end..])
            }
            None => f.write_str(url),
        }
    }
}

/// Like [`redact_url_userinfo`], for a URL that is only `Display`, such as
/// a `GitUrl`.
///
/// Nothing is allocated: `url` is formatted twice, once to find the user
/// name and password and once to write the parts around them.
pub fn redact_displayed_url_userinfo<T: fmt::Display>(url: T) -> RedactDisplayedUrlUserinfo<T> {
    RedactDisplayedUrlUserinfo(url)
}

/// See [`redact_displayed_url_userinfo`].
#[derive(Clone, Copy)]
pub struct RedactDisplayedUrlUserinfo<T>(T);

impl<T: fmt::Display> fmt::Display for RedactDisplayedUrlUserinfo<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut finder = UserinfoFinder::default();
        // Never fails: `UserinfoFinder::write_str` always succeeds.
        let _ = fmt::write(&mut finder, format_args!("{}", self.0));

        match finder.userinfo() {
            Some(userinfo) => fmt::write(
                &mut SkipUserinfo {
                    out: f,
                    pos: 0,
                    userinfo,
                },
                format_args!("{}", self.0),
            ),
            None => self.0.fmt(f),
        }
    }
}

/// Finds the user name and password in a URL: from after the first `://` to
/// the last `@` before the authority ends at a `/`, `?` or `#`.
#[derive(Default)]
struct UserinfoFinder {
    pos: usize,
    /// How much of `://` was just seen, before the authority starts.
    scheme_sep: usize,
    authority_start: Option<usize>,
    authority_ended: bool,
    last_at: Option<usize>,
}

impl UserinfoFinder {
    fn find(url: &str) -> Option<std::ops::Range<usize>> {
        let mut finder = Self::default();
        // Never fails: `UserinfoFinder::write_str` always succeeds.
        let _ = fmt::Write::write_str(&mut finder, url);
        finder.userinfo()
    }

    fn userinfo(&self) -> Option<std::ops::Range<usize>> {
        Some(self.authority_start?..self.last_at?)
    }
}

impl fmt::Write for UserinfoFinder {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if self.authority_ended {
                break;
            }
            if self.authority_start.is_none() {
                self.scheme_sep = match (self.scheme_sep, b) {
                    (_, b':') => 1,
                    (1 | 2, b'/') => self.scheme_sep + 1,
                    _ => 0,
                };
                if self.scheme_sep == 3 {
                    self.authority_start = Some(self.pos + 1);
                }
            } else {
                match b {
                    b'@' => self.last_at = Some(self.pos),
                    b'/' | b'?' | b'#' => self.authority_ended = true,
                    _ => (),
                }
            }
            self.pos += 1;
        }
        Ok(())
    }
}

/// Writes to `out` with `userinfo` replaced.
struct SkipUserinfo<'a, 'b> {
    out: &'a mut fmt::Formatter<'b>,
    pos: usize,
    userinfo: std::ops::Range<usize>,
}

impl fmt::Write for SkipUserinfo<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let start = self.pos;
        let end = start + s.len();
        self.pos = end;

        // Offsets in `s`; the userinfo starts after `://` and ends at `@`,
        // both ASCII, so these are char boundaries.
        let skip_start = self.userinfo.start.clamp(start, end) - start;
        let skip_end = self.userinfo.end.clamp(start, end) - start;

        self.out.write_str(&s[..skip_start])?;
        if (start..end).contains(&self.userinfo.start) {
            self.out.write_str("<redacted>")?;
        }
        self.out.write_str(&s[skip_end..])
    }
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
            assert_eq!(redact_displayed_url_userinfo(url).to_string(), expected);
            // Written in several pieces, as a `Display` impl may do.
            let (a, b) = url.split_at(url.len() / 2);
            let pieces = format_args!("{a}{b}");
            assert_eq!(redact_displayed_url_userinfo(pieces).to_string(), expected);
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
            assert_eq!(redact_displayed_url_userinfo(url).to_string(), url);
        }
    }
}
