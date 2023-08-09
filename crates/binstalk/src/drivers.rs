mod registry;
pub use registry::{
    fetch_crate_cratesio, CratesIoRateLimit, InvalidRegistryError, Registry, RegistryError,
    SparseRegistry,
};

#[cfg(feature = "git")]
pub use registry::{GitRegistry, GitUrl, GitUrlParseError};
