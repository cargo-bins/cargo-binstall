/// DNS resolver used by [`crate::remote::Client`].
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub enum DnsResolver {
    /// Resolve names with the operating system's resolver (`getaddrinfo`).
    System,

    /// Resolve names with the built-in hickory-dns resolver, which reads the
    /// system nameservers itself and falls back to public DNS.
    ///
    /// This needs the `hickory-dns` feature; without it the system resolver
    /// is used instead.
    #[default]
    Hickory,
}
