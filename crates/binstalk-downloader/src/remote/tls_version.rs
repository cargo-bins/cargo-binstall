#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Inner {
    Tls1_2 = 0,
    Tls1_3 = 1,
}

/// TLS version for [`crate::remote::Client`].
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TLSVersion(Inner);

impl TLSVersion {
    pub const TLS_1_2: TLSVersion = TLSVersion(Inner::Tls1_2);
    pub const TLS_1_3: TLSVersion = TLSVersion(Inner::Tls1_3);
}

#[cfg(feature = "__tls")]
impl From<TLSVersion> for reqwest::tls::Version {
    fn from(ver: TLSVersion) -> reqwest::tls::Version {
        use reqwest::tls::Version;
        use Inner::*;

        match ver.0 {
            Tls1_2 => Version::TLS_1_2,
            Tls1_3 => Version::TLS_1_3,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_tls_version_order() {
        assert!(TLSVersion::TLS_1_2 < TLSVersion::TLS_1_3);
    }
}
