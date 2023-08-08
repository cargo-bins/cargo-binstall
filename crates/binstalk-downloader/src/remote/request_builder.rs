use std::fmt;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::Method;

use super::{header, Client, Error, HttpError, StatusCode, Url};

pub use reqwest::Body;

#[cfg(feature = "json")]
pub use serde_json::Error as JsonError;

#[derive(Debug)]
pub struct RequestBuilder {
    pub(super) client: Client,
    pub(super) inner: reqwest::RequestBuilder,
}

impl RequestBuilder {
    pub fn bearer_auth(self, token: &dyn fmt::Display) -> Self {
        Self {
            client: self.client,
            inner: self.inner.bearer_auth(token),
        }
    }

    pub fn header(self, key: &str, value: &str) -> Self {
        Self {
            client: self.client,
            inner: self.inner.header(key, value),
        }
    }

    pub fn body(self, body: impl Into<Body>) -> Self {
        Self {
            client: self.client,
            inner: self.inner.body(body.into()),
        }
    }

    pub async fn send(self, error_for_status: bool) -> Result<Response, Error> {
        let request = self.inner.build()?;
        let method = request.method().clone();
        Ok(Response {
            inner: self.client.send_request(request, error_for_status).await?,
            method,
        })
    }
}

#[derive(Debug)]
pub struct Response {
    inner: reqwest::Response,
    method: Method,
}

impl Response {
    pub async fn bytes(self) -> Result<Bytes, Error> {
        self.inner.bytes().await.map_err(Error::from)
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, Error>> {
        let url = Box::new(self.inner.url().clone());
        let method = self.method;

        self.inner.bytes_stream().map(move |res| {
            res.map_err(|err| {
                Error::Http(Box::new(HttpError {
                    method: method.clone(),
                    url: Url::clone(&*url),
                    err,
                }))
            })
        })
    }

    pub fn status(&self) -> StatusCode {
        self.inner.status()
    }

    pub fn url(&self) -> &Url {
        self.inner.url()
    }

    pub fn method(&self) -> &Method {
        &self.method
    }

    pub fn error_for_status_ref(&self) -> Result<&Self, Error> {
        match self.inner.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(err) => Err(Error::Http(Box::new(HttpError {
                method: self.method().clone(),
                url: self.url().clone(),
                err,
            }))),
        }
    }

    pub fn error_for_status(self) -> Result<Self, Error> {
        match self.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(err) => Err(err),
        }
    }

    pub fn headers(&self) -> &header::HeaderMap {
        self.inner.headers()
    }

    #[cfg(feature = "json")]
    pub async fn json<T>(self) -> Result<T, Error>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = self.error_for_status()?.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}
