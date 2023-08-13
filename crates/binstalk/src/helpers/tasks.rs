use std::{
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::task::JoinHandle;

use crate::errors::BinstallError;

#[derive(Debug)]
pub struct AutoAbortJoinHandle<T>(JoinHandle<T>);

impl<T> AutoAbortJoinHandle<T> {
    pub fn new(handle: JoinHandle<T>) -> Self {
        Self(handle)
    }
}

impl<T> AutoAbortJoinHandle<T>
where
    T: Send + 'static,
{
    pub fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self(tokio::spawn(future))
    }
}

impl<T> Drop for AutoAbortJoinHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Deref for AutoAbortJoinHandle<T> {
    type Target = JoinHandle<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for AutoAbortJoinHandle<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Future for AutoAbortJoinHandle<T> {
    type Output = Result<T, BinstallError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut Pin::into_inner(self).0)
            .poll(cx)
            .map_err(BinstallError::TaskJoinError)
    }
}

impl<T, E> AutoAbortJoinHandle<Result<T, E>>
where
    E: Into<BinstallError>,
{
    pub async fn flattened_join(self) -> Result<T, BinstallError> {
        self.await?.map_err(Into::into)
    }
}
