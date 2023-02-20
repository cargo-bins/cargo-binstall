use std::{ffi::OsStr, fs, io, path::Path};

use compact_str::CompactString;
use reqwest::tls;
use thiserror::Error as ThisError;

use super::ReqwestError;

#[derive(Debug, ThisError)]
pub enum OpenCertificateError {
    #[error(transparent)]
    Reqwest(#[from] ReqwestError),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("Expected extension .pem or .der, but found {0:#?}")]
    UnknownExtensions(Option<CompactString>),
}

#[derive(Clone, Debug)]
pub struct Certificate(pub(super) tls::Certificate);

impl Certificate {
    /// Open Certificate on disk and automatically detect its format based on
    /// its extension.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OpenCertificateError> {
        Self::open_inner(path.as_ref())
    }

    fn open_inner(path: &Path) -> Result<Self, OpenCertificateError> {
        let ext = path.extension();

        let f = if ext == Some(OsStr::new("pem")) {
            Self::from_pem
        } else if ext == Some(OsStr::new("der")) {
            Self::from_der
        } else {
            return Err(OpenCertificateError::UnknownExtensions(
                ext.map(|os_str| os_str.to_string_lossy().into()),
            ));
        };

        Ok(f(fs::read(path)?)?)
    }

    /// Create a Certificate from a binary DER encoded certificate
    pub fn from_der(der: impl AsRef<[u8]>) -> Result<Self, ReqwestError> {
        tls::Certificate::from_der(der.as_ref()).map(Self)
    }

    /// Create a Certificate from a PEM encoded certificate
    pub fn from_pem(pem: impl AsRef<[u8]>) -> Result<Self, ReqwestError> {
        tls::Certificate::from_pem(pem.as_ref()).map(Self)
    }
}
