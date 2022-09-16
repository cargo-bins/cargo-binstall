use once_cell::sync::Lazy;
use url::Url;

pub fn cratesio_url() -> &'static Url {
    static CRATESIO: Lazy<Url, fn() -> Url> =
        Lazy::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

    &CRATESIO
}
