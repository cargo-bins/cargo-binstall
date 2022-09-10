use std::{
    io::Error,
    ops::Deref,
    path::{Path, PathBuf},
};

use once_cell::sync::{Lazy, OnceCell};
use url::Url;

pub fn cargo_home() -> Result<&'static Path, Error> {
    static CARGO_HOME: OnceCell<PathBuf> = OnceCell::new();

    CARGO_HOME
        .get_or_try_init(home::cargo_home)
        .map(Deref::deref)
}

pub fn cratesio_url() -> &'static Url {
    static CRATESIO: Lazy<Url, fn() -> Url> =
        Lazy::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

    &CRATESIO
}
