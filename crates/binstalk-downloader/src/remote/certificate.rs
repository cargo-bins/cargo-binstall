use binstalk_types::errors::Result;
#[cfg(feature = "__tls")]
use reqwest::tls;

#[derive(Clone, Debug)]
pub struct Certificate(#[cfg(feature = "__tls")] pub(super) tls::Certificate);

#[cfg_attr(not(feature = "__tls"), allow(unused_variables))]
impl Certificate {
    /// Create a Certificate from a binary DER encoded certificate
    pub fn from_der(der: impl AsRef<[u8]>) -> Result<Self> {
        #[cfg(not(feature = "__tls"))]
        return Ok(Self());

        #[cfg(feature = "__tls")]
        Ok(Self(tls::Certificate::from_der(der.as_ref())?))
    }

    /// Create a Certificate from a PEM encoded certificate
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Result<Self> {
        #[cfg(not(feature = "__tls"))]
        return Ok(Self());

        #[cfg(feature = "__tls")]
        Ok(Self(tls::Certificate::from_pem(pem.as_ref())?))
    }
}
