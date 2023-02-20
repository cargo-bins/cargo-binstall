use reqwest::tls;

use super::ReqwestError;

#[derive(Clone, Debug)]
pub struct Certificate(pub(super) tls::Certificate);

impl Certificate {
    /// Create a Certificate from a binary DER encoded certificate
    pub fn from_der(der: impl AsRef<[u8]>) -> Result<Self, ReqwestError> {
        tls::Certificate::from_der(der.as_ref()).map(Self)
    }

    /// Create a Certificate from a PEM encoded certificate
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Result<Self, ReqwestError> {
        tls::Certificate::from_pem(pem.as_ref()).map(Self)
    }
}
