use std::{error::Error, fmt};

/// DNS resolver used by [`crate::remote::Client`].
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub enum DnsResolver {
    /// Resolve names with the operating system's resolver (`getaddrinfo`).
    ///
    /// This is the default when the `hickory-dns` feature is disabled.
    #[cfg_attr(not(feature = "hickory-dns"), default)]
    System,

    /// Resolve names with the built-in hickory-dns resolver, which reads the
    /// system nameservers itself and falls back to public DNS.
    ///
    /// Only available with the `hickory-dns` feature, where it is the default.
    #[cfg(feature = "hickory-dns")]
    #[default]
    Hickory,
}

/// A hostname lookup done by the hickory-dns resolver (`DnsResolver::Hickory`) failed.
///
/// It is in the chain of sources of the request's error, so callers can look for it
/// and, for example, suggest [`DnsResolver::System`].
#[derive(Debug)]
pub struct HickoryDnsError(Box<dyn Error + Send + Sync>);

impl HickoryDnsError {
    #[cfg_attr(not(feature = "hickory-dns"), allow(dead_code))]
    pub(super) fn new(err: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self(err.into())
    }
}

impl fmt::Display for HickoryDnsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Error for HickoryDnsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}
