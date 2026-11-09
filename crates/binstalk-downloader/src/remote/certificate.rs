#[cfg(feature = "__tls")]
use reqwest::tls;

#[cfg(feature = "__tls")]
use exn::ResultExt;

use super::Error;

#[derive(Clone, Debug)]
pub struct Certificate(#[cfg(feature = "__tls")] pub(super) tls::Certificate);

#[cfg_attr(not(feature = "__tls"), allow(unused_variables))]
impl Certificate {
    /// Create a Certificate from a binary DER encoded certificate
    pub fn from_der(der: impl AsRef<[u8]>) -> exn::Result<Self, Error> {
        #[cfg(not(feature = "__tls"))]
        return Ok(Self());

        #[cfg(feature = "__tls")]
        tls::Certificate::from_der(der.as_ref())
            .map(Self)
            .or_raise(|| Error::Reqwest)
    }

    /// Create a Certificate from a PEM encoded certificate
    pub fn from_pem(pem: impl AsRef<[u8]>) -> exn::Result<Self, Error> {
        #[cfg(not(feature = "__tls"))]
        return Ok(Self());

        #[cfg(feature = "__tls")]
        tls::Certificate::from_pem(pem.as_ref())
            .map(Self)
            .or_raise(|| Error::Reqwest)
    }
}
