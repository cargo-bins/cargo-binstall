use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use cargo_toml::Manifest;
use futures_util::stream::Stream;
use log::debug;
use reqwest::{tls, Client, ClientBuilder, Method, Response};
use serde::Serialize;
use tinytemplate::TinyTemplate;
use tokio::task::block_in_place;
use url::Url;

pub mod async_extracter;
pub mod download;
pub mod extracter;
pub mod jobserver_client;
pub mod signal;
pub mod statics;
pub mod stream_readable;
pub mod tasks;

use crate::{errors::BinstallError, manifests::cargo_toml_binstall::Meta};

use self::statics::cargo_home;

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path<P: AsRef<Path>>(
    manifest_path: P,
) -> Result<Manifest<Meta>, BinstallError> {
    block_in_place(|| {
        let manifest_path = manifest_path.as_ref();
        let manifest_path = if manifest_path.is_dir() {
            manifest_path.join("Cargo.toml")
        } else if manifest_path.is_file() {
            manifest_path.into()
        } else {
            return Err(BinstallError::CargoManifestPath);
        };

        debug!(
            "Reading manifest at local path: {}",
            manifest_path.display()
        );

        // Load and parse manifest (this checks file system for binary output names)
        let manifest = Manifest::<Meta>::from_path_with_metadata(manifest_path)?;

        // Return metadata
        Ok(manifest)
    })
}

pub fn create_reqwest_client(
    secure: bool,
    min_tls: Option<tls::Version>,
) -> Result<Client, BinstallError> {
    const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

    let mut builder = ClientBuilder::new().user_agent(USER_AGENT);

    if secure {
        builder = builder
            .https_only(true)
            .min_tls_version(tls::Version::TLS_1_2);
    }

    if let Some(ver) = min_tls {
        builder = builder.min_tls_version(ver);
    }

    Ok(builder.build()?)
}

pub async fn remote_exists(
    client: Client,
    url: Url,
    method: Method,
) -> Result<bool, BinstallError> {
    let req = client
        .request(method.clone(), url.clone())
        .send()
        .await
        .map_err(|err| BinstallError::Http { method, url, err })?;
    Ok(req.status().is_success())
}

async fn create_request(
    client: &Client,
    url: Url,
) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, BinstallError> {
    debug!("Downloading from: '{url}'");

    client
        .get(url.clone())
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|err| BinstallError::Http {
            method: Method::GET,
            url,
            err,
        })
        .map(Response::bytes_stream)
}

/// Fetch install path from environment
/// roughly follows <https://doc.rust-lang.org/cargo/commands/cargo-install.html#description>
///
/// Return (install_path, is_custom_install_path)
pub fn get_install_path<P: AsRef<Path>>(install_path: Option<P>) -> (Option<Arc<Path>>, bool) {
    // Command line override first first
    if let Some(p) = install_path {
        return (Some(Arc::from(p.as_ref())), true);
    }

    // Environmental variables
    if let Ok(p) = std::env::var("CARGO_INSTALL_ROOT") {
        debug!("using CARGO_INSTALL_ROOT ({p})");
        let b = PathBuf::from(p);
        return (Some(Arc::from(b.join("bin"))), true);
    }

    if let Ok(p) = cargo_home() {
        debug!("using ({}) as cargo home", p.display());
        return (Some(p.join("bin").into()), false);
    }

    // Local executable dir if no cargo is found
    let dir = dirs::executable_dir();

    if let Some(d) = &dir {
        debug!("Fallback to {}", d.display());
    }

    (dir.map(Arc::from), true)
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
