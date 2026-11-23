use std::{collections::HashMap, iter::Peekable, num::NonZeroU64, sync::Mutex};

use compact_str::{CompactString, ToCompactString};
use reqwest::{Request, Url};
use tokio::time::{Duration, Instant, sleep_until};
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

/// Client-side rate limit: at most `num_request` requests per `per`.
#[derive(Debug)]
struct RateLimit {
    num_request: NonZeroU64,
    per: Duration,
    /// End of the current window.
    until: Instant,
    /// Requests left in the current window.
    rem: u64,
}

impl RateLimit {
    fn new(num_request: NonZeroU64, per: Duration) -> Self {
        Self {
            num_request,
            per,
            until: Instant::now() + per,
            rem: num_request.get(),
        }
    }

    fn inc_rate_limit(&mut self) {
        if let Some(num_request) = NonZeroU64::new(self.num_request.get() / 2) {
            // If self.num_request.get() > 1, then cut it by half
            self.num_request = num_request;
            self.rem = self.rem.min(num_request.get());
        }

        let per = self.per;
        if per < Duration::from_millis(700) {
            self.per = per.mul_f32(1.2);
            self.until += self.per - per;
        }
    }

    /// Take one request from the current window, or return when the next
    /// window starts.
    fn try_acquire(&mut self) -> Result<(), Instant> {
        let now = Instant::now();

        // If the window has elapsed, start a new one.
        if now >= self.until {
            self.until = now + self.per;
            self.rem = self.num_request.get();
        }

        if self.rem == 0 {
            Err(self.until)
        } else {
            self.rem -= 1;
            Ok(())
        }
    }
}

#[derive(Debug)]
pub(super) struct DelayRequest {
    client: reqwest::Client,
    rate_limit: Mutex<RateLimit>,
    /// Held by the request that waits for the client-side rate limit.
    ///
    /// tokio's `Mutex` is fair, so requests take turns in FIFO order and
    /// only the one at the front sleeps: the others wait in the queue
    /// without being woken up each window.
    ///
    /// A request that is dropped while queued simply leaves the queue, so no
    /// window is reserved for it.
    queue: tokio::sync::Mutex<()>,
    hosts_to_delay: Mutex<HashMap<CompactString, Instant>>,
    #[cfg(test)]
    attempts: std::sync::atomic::AtomicU64,
}

impl DelayRequest {
    pub(super) fn new(num_request: NonZeroU64, per: Duration, client: reqwest::Client) -> Self {
        Self {
            client,
            rate_limit: Mutex::new(RateLimit::new(num_request, per)),
            queue: Default::default(),
            hosts_to_delay: Default::default(),
            #[cfg(test)]
            attempts: Default::default(),
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

    fn try_acquire(&self) -> Result<(), Instant> {
        #[cfg(test)]
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        self.rate_limit.lock().unwrap().try_acquire()
    }

    /// Wait until a request to `host` may be sent.
    async fn wait(&self, host: Option<&str>) {
        let mut counter = 0;

        loop {
            let until = {
                // Wait for our turn, then until we are ready to send the
                // request (client-side rate-limit throttler).
                let _turn = self.queue.lock().await;

                loop {
                    // If the host rate-limit us, then wait until then
                    // and try again (server-side rate-limit throttler),
                    // without holding up requests to other hosts.
                    if let Some(until) = host.and_then(|host| self.get_delay_until(host)) {
                        break until;
                    }

                    match self.try_acquire() {
                        Ok(()) => return,
                        Err(until) => sleep_until(until).await,
                    }
                }
            };

            // Try increasing client-side rate-limit throttler to prevent
            // rate-limit in the future.
            self.rate_limit.lock().unwrap().inc_rate_limit();

            let additional_delay =
                Duration::from_millis(200) + Duration::from_millis(100) * 20.min(counter);

            counter += 1;

            debug!("server-side rate limit exceeded; sleeping.");
            sleep_until(until + additional_delay).await;
        }
    }

    pub(super) async fn call(&self, req: Request) -> RequestResult {
        self.wait(req.url().host_str()).await;
        self.client.execute(req).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        pin::pin,
        sync::{Arc, atomic::Ordering},
    };

    use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};

    use super::*;

    const PER: Duration = Duration::from_millis(10);

    fn delay_request(num_request: u64) -> DelayRequest {
        DelayRequest::new(
            NonZeroU64::new(num_request).unwrap(),
            PER,
            reqwest::Client::new(),
        )
    }

    /// Each window lets `num_request` requests through, in FIFO order, and
    /// each queued request is only woken up once.
    #[tokio::test(start_paused = true)]
    async fn test_rate_limit_fifo() {
        let delay_request = Arc::new(delay_request(2));
        let start = Instant::now();

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let delay_request = delay_request.clone();
                tokio::spawn(async move {
                    delay_request.wait(Some("example.com")).await;
                    (i, start.elapsed())
                })
            })
            .collect();

        let mut done = Vec::new();
        for handle in handles {
            done.push(handle.await.unwrap());
        }
        done.sort_by_key(|(_, elapsed)| *elapsed);

        for (n, (i, elapsed)) in done.into_iter().enumerate() {
            assert_eq!(elapsed, PER * (n as u32 / 2), "request {i}");
        }

        // 10 acquired + one failed attempt per window waited for.
        assert_eq!(delay_request.attempts.load(Ordering::Relaxed), 10 + 4);
    }

    /// A request dropped while queued doesn't hold up the ones behind it.
    #[tokio::test(start_paused = true)]
    async fn test_rate_limit_dropped() {
        let delay_request = delay_request(1);
        let start = Instant::now();

        // Use up the current window.
        delay_request.wait(None).await;

        let mut queued: FuturesUnordered<_> =
            (0..100).map(|_| delay_request.wait(None).boxed()).collect();
        // Let them all queue up, then drop them.
        assert!(queued.next().now_or_never().is_none());
        drop(queued);

        delay_request.wait(None).await;
        assert_eq!(start.elapsed(), PER);
    }

    /// A request to a host that rate-limited us waits for it, without holding
    /// up requests to other hosts.
    #[tokio::test(start_paused = true)]
    async fn test_host_delay() {
        let delay_request = delay_request(100);
        let start = Instant::now();

        let url = Url::parse("https://example.com/a").unwrap();
        delay_request.add_urls_to_delay(&[&url], Duration::from_secs(1));

        let mut delayed = pin!(delay_request.wait(Some("example.com")));
        assert!((&mut delayed).now_or_never().is_none());

        delay_request.wait(Some("example.org")).await;
        assert_eq!(start.elapsed(), Duration::ZERO);

        delayed.await;
        assert_eq!(start.elapsed(), Duration::from_millis(1200));
    }
}
