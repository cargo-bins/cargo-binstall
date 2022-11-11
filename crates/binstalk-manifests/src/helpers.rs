use std::{fs, io, path::Path};

use once_cell::sync::Lazy;
use url::Url;

/// Returned file is readable and writable.
pub(crate) fn create_if_not_exist(path: impl AsRef<Path>) -> io::Result<fs::File> {
    let path = path.as_ref();

    let mut options = fs::File::options();
    options.read(true).write(true);

    options
        .clone()
        .create_new(true)
        .open(path)
        .or_else(|_| options.open(path))
}

pub(crate) fn cratesio_url() -> &'static Url {
    static CRATESIO: Lazy<Url, fn() -> Url> =
        Lazy::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

    &CRATESIO
}
