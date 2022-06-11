use std::fmt::Debug;
use std::path::{Path, PathBuf};

use cargo_toml::Manifest;
use log::debug;
use reqwest::{Method, Response};
use serde::Serialize;
use tinytemplate::TinyTemplate;
use url::Url;

use crate::{BinstallError, Meta, PkgFmt, PkgFmtDecomposed, TarBasedFmt};

mod async_extracter;
pub use async_extracter::*;

mod auto_abort_join_handle;
pub use auto_abort_join_handle::AutoAbortJoinHandle;

mod ui_thread;
pub use ui_thread::UIThread;

mod extracter;
pub use extracter::TarEntriesVisitor;

mod readable_rx;

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path<P: AsRef<Path>>(
    manifest_path: P,
) -> Result<Manifest<Meta>, BinstallError> {
    debug!("Reading manifest: {}", manifest_path.as_ref().display());

    // Load and parse manifest (this checks file system for binary output names)
    let manifest = Manifest::<Meta>::from_path_with_metadata(manifest_path)?;

    // Return metadata
    Ok(manifest)
}

pub async fn remote_exists(url: Url, method: Method) -> Result<bool, BinstallError> {
    let req = reqwest::Client::new()
        .request(method.clone(), url.clone())
        .send()
        .await
        .map_err(|err| BinstallError::Http { method, url, err })?;
    Ok(req.status().is_success())
}

async fn create_request(url: Url) -> Result<Response, BinstallError> {
    reqwest::get(url.clone())
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|err| BinstallError::Http {
            method: Method::GET,
            url,
            err,
        })
}

/// Download a file from the provided URL and extract it to the provided path.
pub async fn download_and_extract<P: AsRef<Path>>(
    url: Url,
    fmt: PkgFmt,
    path: P,
) -> Result<(), BinstallError> {
    debug!("Downloading from: '{url}'");

    let resp = create_request(url).await?;

    let path = path.as_ref();
    debug!("Downloading to file: '{}'", path.display());

    let stream = resp.bytes_stream();

    match fmt.decompose() {
        PkgFmtDecomposed::Tar(fmt) => extract_tar_based_stream(stream, path, fmt).await?,
        PkgFmtDecomposed::Bin => extract_bin(stream, path).await?,
        PkgFmtDecomposed::Zip => extract_zip(stream, path).await?,
    }

    debug!("Download OK, written to file: '{}'", path.display());

    Ok(())
}

/// Download a file from the provided URL and extract part of it to
/// the provided path.
///
///  * `filter` - It will pass the path of the file to it
///    and only extract ones which filter returns `true`.
pub async fn download_and_extract_with_filter<
    Filter: FnMut(&Path) -> bool + Send + 'static,
    P: AsRef<Path>,
>(
    url: Url,
    fmt: TarBasedFmt,
    path: P,
    filter: Filter,
) -> Result<(), BinstallError> {
    debug!("Downloading from: '{url}'");

    let resp = create_request(url).await?;

    let path = path.as_ref();
    debug!("Downloading to file: '{}'", path.display());

    let stream = resp.bytes_stream();

    extract_tar_based_stream_with_filter(stream, path, fmt, filter).await?;

    debug!("Download OK, written to file: '{}'", path.display());

    Ok(())
}

/// Download a file from the provided URL and extract part of it to
/// the provided path.
///
///  * `filter` - If Some, then it will pass the path of the file to it
///    and only extract ones which filter returns `true`.
pub async fn download_tar_based_and_visit<
    V: TarEntriesVisitor + Debug + Send + 'static,
    P: AsRef<Path>,
>(
    url: Url,
    fmt: TarBasedFmt,
    path: P,
    visitor: V,
) -> Result<V, BinstallError> {
    debug!("Downloading from: '{url}'");

    let resp = create_request(url).await?;

    let path = path.as_ref();
    debug!("Downloading to file: '{}'", path.display());

    let stream = resp.bytes_stream();

    let visitor = extract_tar_based_stream_and_visit(stream, path, fmt, visitor).await?;

    debug!("Download OK, written to file: '{}'", path.display());

    Ok(visitor)
}

/// Fetch install path from environment
/// roughly follows <https://doc.rust-lang.org/cargo/commands/cargo-install.html#description>
pub fn get_install_path<P: AsRef<Path>>(install_path: Option<P>) -> Option<PathBuf> {
    // Command line override first first
    if let Some(p) = install_path {
        return Some(PathBuf::from(p.as_ref()));
    }

    // Environmental variables
    if let Ok(p) = std::env::var("CARGO_INSTALL_ROOT") {
        debug!("using CARGO_INSTALL_ROOT ({p})");
        let b = PathBuf::from(p);
        return Some(b.join("bin"));
    }
    if let Ok(p) = std::env::var("CARGO_HOME") {
        debug!("using CARGO_HOME ({p})");
        let b = PathBuf::from(p);
        return Some(b.join("bin"));
    }

    // Standard $HOME/.cargo/bin
    if let Some(d) = dirs::home_dir() {
        let d = d.join(".cargo/bin");
        if d.exists() {
            debug!("using $HOME/.cargo/bin");

            return Some(d);
        }
    }

    // Local executable dir if no cargo is found
    let dir = dirs::executable_dir();

    if let Some(d) = &dir {
        debug!("Fallback to {}", d.display());
    }

    dir
}

pub trait Template: Serialize {
    fn render(&self, template: &str) -> Result<String, BinstallError>
    where
        Self: Sized,
    {
        // Create template instance
        let mut tt = TinyTemplate::new();

        // Add template to instance
        tt.add_template("path", template)?;

        // Render output
        Ok(tt.render("path", self)?)
    }
}
