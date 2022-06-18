use std::path::{Path, PathBuf};

use cargo_toml::Manifest;
use log::debug;
use reqwest::Method;
use serde::Serialize;
use tinytemplate::TinyTemplate;
use url::Url;

use crate::{BinstallError, Meta, PkgFmt};

mod async_extracter;
pub use async_extracter::extract_archive_stream;

mod auto_abort_join_handle;
pub use auto_abort_join_handle::AutoAbortJoinHandle;

mod ui_thread;
pub use ui_thread::UIThread;

mod ui_thread_logger;

mod extracter;
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

/// Download a file from the provided URL and extract it to the provided path.
pub async fn download_and_extract<P: AsRef<Path>>(
    url: Url,
    fmt: PkgFmt,
    path: P,
) -> Result<(), BinstallError> {
    download_and_extract_with_filter::<fn(&Path) -> bool, _>(url, fmt, path.as_ref(), None).await
}

/// Download a file from the provided URL and extract part of it to
/// the provided path.
///
///  * `filter` - If Some, then it will pass the path of the file to it
///    and only extract ones which filter returns `true`.
///    Note that this is a best-effort and it only works when `fmt`
///    is not `PkgFmt::Bin` or `PkgFmt::Zip`.
pub async fn download_and_extract_with_filter<
    Filter: FnMut(&Path) -> bool + Send + 'static,
    P: AsRef<Path>,
>(
    url: Url,
    fmt: PkgFmt,
    path: P,
    filter: Option<Filter>,
) -> Result<(), BinstallError> {
    debug!("Downloading from: '{url}'");

    let resp = reqwest::get(url.clone())
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|err| BinstallError::Http {
            method: Method::GET,
            url,
            err,
        })?;

    let path = path.as_ref();
    debug!("Downloading to file: '{}'", path.display());

    extract_archive_stream(resp.bytes_stream(), path, fmt, filter).await?;

    debug!("Download OK, written to file: '{}'", path.display());

    Ok(())
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
