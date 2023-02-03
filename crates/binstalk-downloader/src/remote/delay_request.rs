use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

use compact_str::{CompactString, ToCompactString};
use pin_project::pin_project;
use reqwest::Request;
use tokio::time::{sleep_until, Instant, Sleep};
use tower::Service;

pub(super) struct DelayRequest<S> {
    inner: S,
    hosts_to_delay: HashMap<CompactString, Instant>,
}

impl<S> DelayRequest<S> {
    pub(super) fn new(inner: S) -> Self {
        Self {
            inner,
            hosts_to_delay: Default::default(),
        }
    }

    pub(super) fn add_host_to_delay(&mut self, host: &str, deadline: Instant) {
        self.hosts_to_delay
            .entry(host.to_compact_string())
            .and_modify(|old_dl| {
                *old_dl = deadline.max(*old_dl);
            })
            .or_insert(deadline);
    }
}

impl<S> Service<Request> for DelayRequest<S>
where
    S: Service<Request>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = DelayRequestFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let sleep = req
            .url()
            .host_str()
            .and_then(|host| {
                self.hosts_to_delay
                    .get(host)
                    .map(|deadline| (*deadline, host))
            })
            .and_then(|(deadline, host)| {
                if deadline.elapsed().is_zero() {
                    Some(Box::pin(sleep_until(deadline)))
                } else {
                    // We have already gone past the deadline,
                    // so we should remove it instead.
                    self.hosts_to_delay.remove(host);
                    None
                }
            });

        DelayRequestFuture {
            sleep,
            inner: self.inner.call(req),
        }
    }
}

#[pin_project]
pub(super) struct DelayRequestFuture<F> {
    sleep: Option<Pin<Box<Sleep>>>,

    #[pin]
    inner: F,
}

impl<F> Future for DelayRequestFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        if let Some(sleep) = this.sleep.as_mut() {
            ready!(sleep.as_mut().poll(cx));
            *this.sleep = None;
        }

        this.inner.poll(cx)
    }
}
