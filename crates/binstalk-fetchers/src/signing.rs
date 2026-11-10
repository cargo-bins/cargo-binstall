use binstalk_downloader::download::DataVerifier;
use binstalk_types::cargo_toml_binstall::{PkgSigning, SigningAlgorithm};
use binstalk_types::{
    errors::{Exn, Result},
    message,
};
use bytes::Bytes;
use minisign_verify::{PublicKey, Signature, StreamVerifier};
use tracing::{error, trace};

use crate::invalid_signature;

pub enum SignatureVerifier {
    Noop,
    Minisign(Box<MinisignVerifier>),
}

impl SignatureVerifier {
    pub fn new(config: &PkgSigning, signature: &[u8]) -> Result<Self> {
        match config.algorithm {
            SigningAlgorithm::Minisign => MinisignVerifier::new(config, signature)
                .map(Box::new)
                .map(Self::Minisign),
            algorithm => Err(Exn::from(message!(
                "Signing algorithm not supported: {algorithm:?}"
            ))),
        }
    }

    pub fn data_verifier(&self) -> Result<Box<dyn DataVerifier + '_>> {
        match self {
            Self::Noop => Ok(Box::new(())),
            Self::Minisign(v) => v.data_verifier(),
        }
    }

    pub fn info(&self) -> Option<String> {
        match self {
            Self::Noop => None,
            Self::Minisign(v) => Some(v.signature.trusted_comment().into()),
        }
    }
}

pub struct MinisignVerifier {
    pubkey: PublicKey,
    signature: Signature,
}

impl MinisignVerifier {
    pub fn new(config: &PkgSigning, signature: &[u8]) -> Result<Self> {
        trace!(key=?config.pubkey, "parsing public key");
        let pubkey = PublicKey::from_base64(&config.pubkey).map_err(|err| {
            error!("Package public key is invalid: {err}");
            invalid_signature()
        })?;

        trace!(?signature, "parsing signature");
        let signature = Signature::decode(std::str::from_utf8(signature).map_err(|err| {
            error!(?signature, "Signature file is not UTF-8! {err}");
            invalid_signature()
        })?)
        .map_err(|err| {
            error!("Signature file is invalid: {err}");
            invalid_signature()
        })?;

        Ok(Self { pubkey, signature })
    }

    pub fn data_verifier(&self) -> Result<Box<dyn DataVerifier + '_>> {
        self.pubkey
            .verify_stream(&self.signature)
            .map(|vs| Box::new(MinisignDataVerifier(vs)) as _)
            .map_err(|err| {
                error!("Failed to setup stream verifier: {err}");
                Exn::from(invalid_signature())
            })
    }
}

pub struct MinisignDataVerifier<'a>(StreamVerifier<'a>);

impl DataVerifier for MinisignDataVerifier<'_> {
    fn update(&mut self, data: &Bytes) {
        self.0.update(data);
    }

    fn validate(&mut self) -> bool {
        if let Err(err) = self.0.finalize() {
            error!("Failed to finalize signature verify: {err}");
            false
        } else {
            true
        }
    }
}
