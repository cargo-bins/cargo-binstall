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
    /// Errors from hickory-dns are returned as is.
    ///
    /// Only available with the `hickory-dns` feature.
    #[cfg(feature = "hickory-dns")]
    Hickory,

    /// Resolve names with hickory-dns as [`DnsResolver::Hickory`] does, but retry
    /// a lookup that fails with the operating system's resolver (`getaddrinfo`),
    /// which helps on hosts where hickory-dns cannot reach the nameservers, e.g.
    /// behind some VPNs or in containers that rely on NSS.
    ///
    /// A nameserver answering that the name has no address (NXDOMAIN or no
    /// records) is not retried.
    ///
    /// Only available with the `hickory-dns` feature, where it is the default.
    #[cfg(feature = "hickory-dns")]
    #[default]
    HickoryWithSystemFallback,
}
