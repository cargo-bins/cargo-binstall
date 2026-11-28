use super::*;

/// Fixtures made by `tests/openpgp/generate.sh`, with GnuPG and Sequoia.
const MESSAGE: &[u8] = include_bytes!("../../../tests/openpgp/message");

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("../../../tests/openpgp/", $name)).as_slice()
    };
}

macro_rules! key {
    ($name:literal) => {
        include_str!(concat!("../../../tests/openpgp/", $name, ".asc"))
    };
}

fn verify(key: &str, signature: &[u8], message: &[u8]) -> Result<bool, FetchError> {
    let verifier = OpenPgpVerifier::new(key, signature)?;
    let mut data_verifier = verifier.data_verifier();
    // Feed in two chunks to exercise the streaming path.
    let (a, b) = message.split_at(message.len() / 2);
    data_verifier.update(&Bytes::copy_from_slice(a));
    data_verifier.update(&Bytes::copy_from_slice(b));
    Ok(data_verifier.validate())
}

const VALID: &[(&str, &[u8])] = &[
    (key!("ed25519"), fixture!("ed25519.sig")),
    (key!("ed25519"), fixture!("ed25519.sig.asc")),
    (key!("rsa3072"), fixture!("rsa3072.sig")),
    (key!("nistp256"), fixture!("nistp256.sig")),
    (key!("nistp256"), fixture!("nistp256-sha512.sig")),
    (key!("nistp384"), fixture!("nistp384.sig")),
    (key!("nistp521"), fixture!("nistp521.sig")),
    (key!("subkey"), fixture!("subkey.sig")),
    (key!("ed25519"), fixture!("two-signers.sig")),
    (key!("rsa3072"), fixture!("two-signers.sig")),
    (key!("v6"), fixture!("v6.sig")),
];

#[test]
fn valid_signatures() {
    for (i, (key, sig)) in VALID.iter().enumerate() {
        assert!(verify(key, sig, MESSAGE).unwrap(), "fixture {i}");
    }
}

#[test]
fn tampered_message() {
    let mut message = MESSAGE.to_vec();
    message.push(b'!');
    for (i, (key, sig)) in VALID.iter().enumerate() {
        assert!(!verify(key, sig, &message).unwrap(), "fixture {i}");
    }
}

#[test]
fn info() {
    let verifier = OpenPgpVerifier::new(key!("subkey"), fixture!("subkey.sig")).unwrap();
    let info = verifier.info();
    assert!(
        info.starts_with("OpenPGP key ")
            && info.ends_with(" (binstall-test-subkey <subkey@example.com>)"),
        "{info}"
    );
    // The primary key's fingerprint, not the signing subkey's.
    assert_eq!(hex_upper(&verifier.keys[0].fingerprint).len(), 40);
    assert!(info.contains(&hex_upper(&verifier.keys[0].fingerprint)));

    let verifier = OpenPgpVerifier::new(key!("v6"), fixture!("v6.sig")).unwrap();
    assert_eq!(verifier.keys[0].fingerprint.len(), 32);
}

#[test]
fn wrong_key() {
    assert!(OpenPgpVerifier::new(key!("ed25519"), fixture!("rsa3072.sig")).is_err());
    assert!(OpenPgpVerifier::new(key!("nistp256"), fixture!("ed25519.sig")).is_err());
    assert!(OpenPgpVerifier::new(key!("ed25519"), fixture!("subkey.sig")).is_err());
}

#[test]
fn rejected_signatures() {
    // SHA-1, text mode, and an unsupported curve.
    assert!(OpenPgpVerifier::new(key!("ed25519"), fixture!("ed25519-sha1.sig")).is_err());
    assert!(OpenPgpVerifier::new(key!("ed25519"), fixture!("ed25519-text.sig")).is_err());
    assert!(OpenPgpVerifier::new(key!("brainpool"), fixture!("brainpool.sig")).is_err());
}

#[test]
fn malformed() {
    let key = key!("ed25519");
    let sig = fixture!("ed25519.sig");
    assert!(OpenPgpVerifier::new(key, b"").is_err());
    assert!(OpenPgpVerifier::new(key, &sig[..sig.len() - 1]).is_err());
    assert!(OpenPgpVerifier::new("", sig).is_err());
    assert!(OpenPgpVerifier::new(&key.replace("PUBLIC KEY", "PRIVATE KEY"), sig).is_err());
    // A minisign signature.
    assert!(OpenPgpVerifier::new(key, b"untrusted comment: x\nRUQ=\n").is_err());
}

#[test]
fn der() {
    assert_eq!(
        der_signature(&[1], &[0x80]),
        [0x30, 7, 2, 1, 1, 2, 2, 0, 0x80]
    );
    assert_eq!(der_signature(&[0, 0, 1], &[]), [0x30, 6, 2, 1, 1, 2, 1, 0]);
    let long = der_signature(&[0x7F; 66], &[0xFF; 66]);
    assert_eq!(&long[..3], [0x30, 0x81, 137]);
}
