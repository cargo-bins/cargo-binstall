use std::{
    future::Future,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use exn::{Exn, ResultExt};
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
    type Output = exn::Result<T, BinstallError>;

    #[track_caller]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut Pin::into_inner(self).0)
            .poll(cx)
            .map(|res| res.or_raise(|| BinstallError::TaskJoinError))
    }
}

impl<T> AutoAbortJoinHandle<Result<T, Exn<BinstallError>>> {
    pub async fn flattened_join(self) -> exn::Result<T, BinstallError> {
        self.await?
    }
}
