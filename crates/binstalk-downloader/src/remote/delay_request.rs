use std::{
    collections::HashMap, future::Future, iter::Peekable, num::NonZeroU64, ops::ControlFlow,
    sync::Mutex,
};

use compact_str::{CompactString, ToCompactString};
use reqwest::{Request, Url};
use tokio::time::{sleep_until, Duration, Instant};
use tracing::debug;

pub(super) type RequestResult = Result<reqwest::Response, reqwest::Error>;

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
struct Inner {
    client: reqwest::Client,
    num_request: NonZeroU64,
    per: Duration,
    until: Instant,
    state: State,
}

#[derive(Debug)]
enum State {
    Limited,
    Ready { rem: NonZeroU64 },
}

impl Inner {
    fn new(num_request: NonZeroU64, per: Duration, client: reqwest::Client) -> Self {
        Inner {
            client,
            per,
            num_request,
            until: Instant::now() + per,
            state: State::Ready { rem: num_request },
        }
    }

    fn inc_rate_limit(&mut self) {
        if let Some(num_request) = NonZeroU64::new(self.num_request.get() / 2) {
            // If self.num_request.get() > 1, then cut it by half
            self.num_request = num_request;
            if let State::Ready { rem, .. } = &mut self.state {
                *rem = num_request.min(*rem)
            }
        }

        let per = self.per;
        if per < Duration::from_millis(700) {
            self.per = per.mul_f32(1.2);
            self.until += self.per - per;
        }
    }

    fn ready(&mut self) -> Readiness {
        match self.state {
            State::Ready { .. } => Readiness::Ready,
            State::Limited => {
                if self.until.elapsed().is_zero() {
                    Readiness::Limited(self.until)
                } else {
                    // rate limit can be reset now and is ready
                    self.until = Instant::now() + self.per;
                    self.state = State::Ready {
                        rem: self.num_request,
                    };

                    Readiness::Ready
                }
            }
        }
    }

    fn call(&mut self, req: Request) -> impl Future<Output = RequestResult> {
        match &mut self.state {
            State::Ready { rem } => {
                let now = Instant::now();

                // If the period has elapsed, reset it.
                if now >= self.until {
                    self.until = now + self.per;
                    *rem = self.num_request;
                }

                if let Some(new_rem) = NonZeroU64::new(rem.get() - 1) {
                    *rem = new_rem;
                } else {
                    // The service is disabled until further notice
                    self.state = State::Limited;
                }

                // Call the inner future
                self.client.execute(req)
            }
            State::Limited => panic!("service not ready; poll_ready must be called first"),
        }
    }
}

enum Readiness {
    Limited(Instant),
    Ready,
}

#[derive(Debug)]
pub(super) struct DelayRequest {
    inner: Mutex<Inner>,
    hosts_to_delay: Mutex<HashMap<CompactString, Instant>>,
}

impl DelayRequest {
    pub(super) fn new(num_request: NonZeroU64, per: Duration, client: reqwest::Client) -> Self {
        Self {
            inner: Mutex::new(Inner::new(num_request, per, client)),
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

    fn get_delay_until(&self, host: &str) -> Option<Instant> {
        let mut hosts_to_delay = self.hosts_to_delay.lock().unwrap();

        hosts_to_delay.get(host).copied().and_then(|until| {
            if until.elapsed().is_zero() {
                Some(until)
            } else {
                // We have already gone past the deadline,
                // so we should remove it instead.
                hosts_to_delay.remove(host);
                None
            }
        })
    }

    // Define a new function so that the guard will be dropped ASAP and not
    // included in the future.
    fn call_inner(
        &self,
        counter: &mut u32,
        req: &mut Option<Request>,
    ) -> ControlFlow<impl Future<Output = RequestResult>, Instant> {
        // Wait until we are ready to send next requests
        // (client-side rate-limit throttler).
        let mut guard = self.inner.lock().unwrap();

        if let Readiness::Limited(until) = guard.ready() {
            ControlFlow::Continue(until)
        } else if let Some(until) = req
            .as_ref()
            .unwrap()
            .url()
            .host_str()
            .and_then(|host| self.get_delay_until(host))
        {
            // If the host rate-limit us, then wait until then
            // and try again (server-side rate-limit throttler).

            // Try increasing client-side rate-limit throttler to prevent
            // rate-limit in the future.
            guard.inc_rate_limit();

            let additional_delay =
                Duration::from_millis(200) + Duration::from_millis(100) * 20.min(*counter);

            *counter += 1;

            debug!("server-side rate limit exceeded; sleeping.");
            ControlFlow::Continue(until + additional_delay)
        } else {
            ControlFlow::Break(guard.call(req.take().unwrap()))
        }
    }

    pub(super) async fn call(&self, req: Request) -> RequestResult {
        // Put all variables in a block so that will be dropped before polling
        // the future returned by reqwest.
        {
            let mut counter = 0;
            // Use Option here so that we don't have to move entire `Request`
            // twice when calling `self.call_inner` while retain the ability to
            // take its value without boxing.
            //
            // This will be taken when `ControlFlow::Break` is then it will
            // break the loop, so it will never call `self.call_inner` with
            // a `None`.
            let mut req = Some(req);

            loop {
                match self.call_inner(&mut counter, &mut req) {
                    ControlFlow::Continue(until) => sleep_until(until).await,
                    ControlFlow::Break(future) => break future,
                }
            }
        }
        .await
    }
}
