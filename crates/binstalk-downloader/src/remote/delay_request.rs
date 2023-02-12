use std::{
    collections::HashMap,
    future::Future,
    iter::Peekable,
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
};

use compact_str::{CompactString, ToCompactString};
use reqwest::{Request, Url};
use tokio::{
    sync::Mutex as AsyncMutex,
    time::{sleep_until, Duration, Instant},
};
use tower::{Service, ServiceExt};

trait IterExt: Iterator {
    fn dedup(self) -> Dedup<Self>
    where
        Self: Sized,
        Self::Item: PartialEq,
    {
        Dedup(self.peekable())
    }
}

impl<It: Iterator> IterExt for It {}

struct Dedup<It: Iterator>(Peekable<It>);

impl<It> Iterator for Dedup<It>
where
    It: Iterator,
    It::Item: PartialEq,
{
    type Item = It::Item;

    fn next(&mut self) -> Option<Self::Item> {
        let curr = self.0.next()?;

        // Drop all consecutive dup values
        while self.0.next_if_eq(&curr).is_some() {}

        Some(curr)
    }
}

#[derive(Debug)]
pub(super) struct DelayRequest<S> {
    inner: AsyncMutex<S>,
    hosts_to_delay: Mutex<HashMap<CompactString, Instant>>,
}

impl<S> DelayRequest<S> {
    pub(super) fn new(inner: S) -> Self {
        Self {
            inner: AsyncMutex::new(inner),
            hosts_to_delay: Default::default(),
        }
    }

    pub(super) fn add_urls_to_delay(&self, urls: &[&Url], delay_duration: Duration) {
        let deadline = Instant::now() + delay_duration;

        let mut hosts_to_delay = self.hosts_to_delay.lock().unwrap();

        urls.iter()
            .filter_map(|url| url.host_str())
            .dedup()
            .for_each(|host| {
                hosts_to_delay
                    .entry(host.to_compact_string())
                    .and_modify(|old_dl| {
                        *old_dl = deadline.max(*old_dl);
                    })
                    .or_insert(deadline);
            });
    }

    fn wait_until_available(&self, url: &Url) -> impl Future<Output = ()> + Send + 'static {
        let mut hosts_to_delay = self.hosts_to_delay.lock().unwrap();

        let deadline = url
            .host_str()
            .and_then(|host| hosts_to_delay.get(host).map(|deadline| (*deadline, host)))
            .and_then(|(deadline, host)| {
                if deadline.elapsed().is_zero() {
                    Some(deadline)
                } else {
                    // We have already gone past the deadline,
                    // so we should remove it instead.
                    hosts_to_delay.remove(host);
                    None
                }
            });

        async move {
            if let Some(deadline) = deadline {
                sleep_until(deadline).await;
            }
        }
    }
}

impl<'this, S> Service<Request> for &'this DelayRequest<S>
where
    S: Service<Request> + Send,
    S::Future: Send,
{
    type Response = S::Response;
    type Error = S::Error;
    // TODO: Replace this with `type_alias_impl_trait` once it stablises
    // https://github.com/rust-lang/rust/issues/63063
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'this>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let this = *self;

        Box::pin(async move {
            this.wait_until_available(req.url()).await;

            // Reduce critical section:
            //  - Construct the request before locking
            //  - Once it is ready, call it and obtain
            //    the future, then release the lock before
            //    polling the future, which performs network I/O that could
            //    take really long.
            let future = this.inner.lock().await.ready().await?.call(req);

            future.await
        })
    }
}
