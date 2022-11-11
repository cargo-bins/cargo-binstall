use std::{path::Path, sync::Arc};

use compact_str::CompactString;
pub use gh_crate_meta::*;
pub use quickinstall::*;

use crate::{
    errors::BinstallError,
    helpers::remote::Client,
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

pub(crate) mod gh_crate_meta;
pub(crate) mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    #[allow(clippy::new_ret_no_self)]
    fn new(client: &Client, data: &Arc<Data>) -> Arc<dyn Fetcher>
    where
        Self: Sized;

    /// Fetch a package and extract
    async fn fetch_and_extract(&self, dst: &Path) -> Result<(), BinstallError>;

    /// Find the package, if it is available for download
    ///
    /// This may look for multiple remote targets, but must write (using some form of interior
    /// mutability) the best one to the implementing struct in some way so `fetch_and_extract` can
    /// proceed without additional work.
    ///
    /// Must return `true` if a package is available, `false` if none is, and reserve errors to
    /// fatal conditions only.
    async fn find(&self) -> Result<bool, BinstallError>;

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// Return finalized target meta.
    fn target_meta(&self) -> PkgMeta;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> CompactString;

    /// A short human-readable name, must contains only characters
    /// and numbers and it also must be unique.
    ///
    /// It is used to create a temporary dir where it is used for
    /// [`Fetcher::fetch_and_extract`].
    fn fetcher_name(&self) -> &'static str;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;

    /// Return the target for this fetcher
    fn target(&self) -> &str;
}

/// Data required to fetch a package
#[derive(Clone, Debug)]
pub struct Data {
    pub name: String,
    pub target: String,
    pub version: String,
    pub repo: Option<String>,
    pub meta: PkgMeta,
}
