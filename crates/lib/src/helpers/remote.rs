use std::env;

use bytes::Bytes;
use futures_util::stream::Stream;
use log::debug;
use reqwest::{tls, Client, ClientBuilder, Method, Response};
use url::Url;

use crate::errors::BinstallError;

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

pub(crate) async fn create_request(
    client: Client,
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
