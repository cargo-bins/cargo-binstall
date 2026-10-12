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
