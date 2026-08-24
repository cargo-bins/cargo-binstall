//! Continuing a download whose body stopped early.
//!
//! A large artefact fetched over a slow or lossy link can lose its connection
//! after minutes of transfer. Starting again from the first byte costs as long
//! again, and on a link bad enough, longer than it stays up for. So a body that
//! stops before the end is continued with a `Range` request from the byte the
//! caller has already been given, rather than restarted.
//!
//! The continuation is spliced into the stream the caller is already reading, so
//! a download that resumed reads the same as one that never broke: every byte
//! yielded once, in order. That is what lets it stay invisible to the
//! extractors, which decode the octet stream as it arrives and cannot be
//! rewound, and to [`DataVerifier`](super::DataVerifier), which digests
//! whatever it is handed.
//!
//! A server that answers with anything other than a `206` continuing from the
//! right offset cannot be spliced from: it doesn't serve ranges, or `If-Range`
//! told it the artefact has changed since we started reading. Then the error
//! that broke the transfer stands.

use std::{pin::Pin, time::Duration};

use bytes::Bytes;
use futures_util::{stream::unfold, Stream, StreamExt};
use tracing::{debug, warn};

use crate::remote::{
    header::{
        HeaderMap, HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, ETAG, IF_RANGE,
        LAST_MODIFIED, RANGE,
    },
    Client, Error as RemoteError, Response, StatusCode, Url,
};

/// Continuations allowed before a broken download is given up on.
///
/// Refilled whenever a continuation delivers bytes, so this bounds how many
/// times in a row a transfer may fail without moving, not how many times a long
/// download over a lossy link may be picked up.
const MAX_RESUMES: u8 = 5;

/// Wait before re-requesting, so a link or server that has just dropped a
/// connection isn't immediately asked for another.
const RESUME_DELAY: Duration = Duration::from_millis(500);

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, RemoteError>> + Send + Sync>>;

/// Read `response`'s body, continuing it with `Range` requests to `url` when it
/// stops before the end.
pub(super) fn resumable_stream(
    client: Client,
    url: Url,
    response: Response,
) -> impl Stream<Item = Result<Bytes, RemoteError>> + Send + Sync {
    let headers = response.headers();
    let len = content_length(headers);
    let validator = headers
        .get(ETAG)
        .or_else(|| headers.get(LAST_MODIFIED))
        .cloned();
    let budget = budget_for(headers);

    let state = Resumable {
        client,
        url,
        inner: Box::pin(response.bytes_stream()),
        read: 0,
        len,
        validator,
        budget,
        spent_at: 0,
    };

    unfold(Some(state), |state| async move {
        let mut state = state?;
        loop {
            match state.inner.next().await {
                Some(Ok(bytes)) => {
                    state.read += bytes.len() as u64;
                    return Some((Ok(bytes), Some(state)));
                }
                Some(Err(err)) => {
                    warn!(
                        ?err,
                        url = %state.url,
                        offset = state.read,
                        "download broke; trying to continue it",
                    );
                    if !state.resume().await {
                        return Some((Err(err), None));
                    }
                }
                // A body can also stop early without erroring, which the
                // promised length gives us away to notice.
                None if state.is_short() => {
                    warn!(
                        url = %state.url,
                        read = state.read,
                        expected = ?state.len,
                        "download ended early; trying to continue it",
                    );
                    if !state.resume().await {
                        return Some((Err(state.truncated()), None));
                    }
                }
                None => return None,
            }
        }
    })
}

struct Resumable {
    client: Client,
    url: Url,
    /// The body currently being read.
    inner: ByteStream,
    /// Bytes given to the caller, and so the offset a continuation starts at.
    read: u64,
    /// What the artefact measures in full, when the server said.
    len: Option<u64>,
    /// The first response's `ETag` or `Last-Modified`, handed back as
    /// `If-Range` so a server that has replaced the artefact since answers with
    /// the whole of the new one rather than a range that would splice into the
    /// old one.
    validator: Option<HeaderValue>,
    budget: u8,
    /// `read` when the budget was last spent, to tell progress from a stall.
    spent_at: u64,
}

impl Resumable {
    /// Continue the transfer from [`Self::read`], reporting whether it can go
    /// on. When it can't, the error that broke the body stands.
    async fn resume(&mut self) -> bool {
        let made_progress = self.read > self.spent_at;
        self.spent_at = self.read;
        if !spend(&mut self.budget, made_progress) {
            warn!(url = %self.url, "download cannot be continued; out of attempts");
            return false;
        }

        tokio::time::sleep(RESUME_DELAY).await;

        let range = format!("bytes={}-", self.read);
        let mut request = self
            .client
            .get(self.url.clone())
            .header(RANGE.as_str(), &range);
        if let Some(validator) = self.validator.as_ref().and_then(|v| v.to_str().ok()) {
            request = request.header(IF_RANGE.as_str(), validator);
        }

        let response = match request.send(true).await {
            Ok(response) => response,
            Err(err) => {
                warn!(?err, url = %self.url, "download cannot be continued");
                return false;
            }
        };

        let status = response.status();
        if !continues_from(status, response.headers().get(CONTENT_RANGE), self.read) {
            warn!(
                url = %self.url,
                %status,
                offset = self.read,
                "download cannot be continued; the answer is not the rest of it",
            );
            return false;
        }

        debug!(url = %self.url, offset = self.read, "continuing download");
        self.inner = Box::pin(response.bytes_stream());
        true
    }

    /// Whether the body stopped short of the length the first response promised.
    fn is_short(&self) -> bool {
        matches!(self.len, Some(len) if self.read < len)
    }

    fn truncated(&self) -> RemoteError {
        RemoteError::Truncated {
            url: Box::new(self.url.clone()),
            read: self.read,
            expected: self.len.unwrap_or(self.read),
        }
    }
}

/// How many continuations a server's `Accept-Ranges` earns it. One that says it
/// serves no ranges is taken at its word; anything else, including saying
/// nothing at all, is worth an ask if a body breaks.
fn budget_for(headers: &HeaderMap) -> u8 {
    match headers.get(ACCEPT_RANGES) {
        Some(value) if value.as_bytes().eq_ignore_ascii_case(b"none") => 0,
        _ => MAX_RESUMES,
    }
}

/// Take one from the budget, refilling it first if the transfer has moved on
/// since the last attempt. Returns whether there was one to take.
fn spend(budget: &mut u8, made_progress: bool) -> bool {
    if made_progress {
        *budget = MAX_RESUMES;
    }
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    true
}

/// Whether a response carries the rest of a body already partly read, rather
/// than the start of one.
///
/// Only a `206` whose range begins exactly where the caller left off can be
/// spliced on. A server that ignores `Range`, or whose `If-Range` check found
/// the artefact changed, sends the whole thing with a `200`, which is only
/// usable when nothing has been read yet.
fn continues_from(status: StatusCode, content_range: Option<&HeaderValue>, offset: u64) -> bool {
    if status != StatusCode::PARTIAL_CONTENT {
        return offset == 0 && status.is_success();
    }

    content_range_start(content_range) == Some(offset)
}

/// The first-byte position of a `Content-Range: bytes <start>-<end>/<total>`.
fn content_range_start(header: Option<&HeaderValue>) -> Option<u64> {
    let range = header?.to_str().ok()?.trim().strip_prefix("bytes ")?;
    range.trim().split('-').next()?.trim().parse().ok()
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        io::{self, Read, Write},
        net::{TcpListener, TcpStream},
        num::NonZeroU16,
        thread::{self, JoinHandle},
        time::Instant,
    };

    use crate::download::Download;

    fn header(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).unwrap()
    }

    fn test_client() -> Client {
        Client::new(
            "cargo-binstall-test",
            None,
            true,
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    /// Twenty bytes promised, ten delivered, then the connection goes away.
    const CUT_SHORT: &[u8] = b"HTTP/1.1 200 OK\r\n\
        Content-Length: 20\r\n\
        Accept-Ranges: bytes\r\n\
        ETag: \"v1\"\r\n\
        Connection: close\r\n\r\n\
        0123456789";

    /// The ten bytes that finish it.
    const THE_REST: &[u8] = b"HTTP/1.1 206 Partial Content\r\n\
        Content-Length: 10\r\n\
        Content-Range: bytes 10-19/20\r\n\
        Connection: close\r\n\r\n\
        abcdefghij";

    /// All twenty bytes again, and different ones: what a server sends when it
    /// serves no ranges, or when `If-Range` finds the artefact has changed.
    const THE_WHOLE_THING_AGAIN: &[u8] = b"HTTP/1.1 200 OK\r\n\
        Content-Length: 20\r\n\
        Connection: close\r\n\r\n\
        ABCDEFGHIJKLMNOPQRST";

    /// How long to keep waiting for a request that may never come, once the
    /// client has been given a body it could ask to have continued. Longer than
    /// [`RESUME_DELAY`], so a client that does ask is not missed.
    const QUIET_FOR: Duration = Duration::from_millis(2000);

    /// Answer each canned response with one request, and report the requests
    /// that arrived.
    ///
    /// Stops waiting once nothing more comes, so a client that asks for fewer
    /// requests than there are responses leaves a test that can still be joined.
    fn serve(
        listener: TcpListener,
        responses: &'static [&'static [u8]],
    ) -> JoinHandle<Vec<String>> {
        thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let mut requests = Vec::new();

            for response in responses {
                let mut stream = match accept_before(&listener, Instant::now() + QUIET_FOR) {
                    Some(stream) => stream,
                    None => break,
                };
                requests.push(read_request(&mut stream));
                stream.write_all(response).unwrap();
                stream.flush().unwrap();
            }

            requests
        })
    }

    fn accept_before(listener: &TcpListener, deadline: Instant) -> Option<TcpStream> {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    return Some(stream);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
    }

    /// Read one request off the wire, up to the end of its headers.
    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap() == 0 {
                break;
            }
            request.push(byte[0]);
        }
        String::from_utf8_lossy(&request).into_owned()
    }

    #[tokio::test]
    async fn a_body_cut_short_is_continued_from_where_it_stopped() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve(listener, &[CUT_SHORT, THE_REST]);

        let url = Url::parse(&format!("http://{addr}/")).unwrap();
        let bytes = Download::new(test_client(), url)
            .into_bytes()
            .await
            .unwrap();

        // The caller sees one unbroken body, and each byte once.
        assert_eq!(&bytes[..], b"0123456789abcdefghij");

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        let asked_again = requests[1].to_ascii_lowercase();
        assert!(
            asked_again.contains("range: bytes=10-"),
            "expected the rest to be asked for, got:\n{}",
            requests[1]
        );
        assert!(
            asked_again.contains("if-range: \"v1\""),
            "expected the validator to be handed back, got:\n{}",
            requests[1]
        );
    }

    #[tokio::test]
    async fn a_whole_body_offered_mid_transfer_is_refused_rather_than_spliced() {
        // Joining that to the ten bytes already handed over would produce a
        // file that is neither the old artefact nor the new one, so the
        // transfer fails instead.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve(listener, &[CUT_SHORT, THE_WHOLE_THING_AGAIN]);

        let url = Url::parse(&format!("http://{addr}/")).unwrap();
        let res = Download::new(test_client(), url).into_bytes().await;

        assert!(res.is_err(), "expected a failure, got {res:?}");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn without_resume_the_body_is_read_in_one_request() {
        // The same server that completes the download above: the difference is
        // that nothing asks it for the rest.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = serve(listener, &[CUT_SHORT, THE_REST]);

        let url = Url::parse(&format!("http://{addr}/")).unwrap();
        let res = Download::new(test_client(), url)
            .without_resume()
            .into_bytes()
            .await;

        assert!(res.is_err(), "expected a failure, got {res:?}");
        assert_eq!(server.join().unwrap().len(), 1);
    }

    #[test]
    fn partial_content_continuing_at_the_offset_splices() {
        let range = header("bytes 4096-8191/8192");
        assert!(continues_from(
            StatusCode::PARTIAL_CONTENT,
            Some(&range),
            4096
        ));
    }

    #[test]
    fn partial_content_starting_elsewhere_does_not_splice() {
        // Splicing this on would leave a hole, or repeat bytes the caller has
        // already digested.
        let range = header("bytes 0-8191/8192");
        assert!(!continues_from(
            StatusCode::PARTIAL_CONTENT,
            Some(&range),
            4096
        ));
    }

    #[test]
    fn partial_content_without_a_range_does_not_splice() {
        assert!(!continues_from(StatusCode::PARTIAL_CONTENT, None, 4096));
    }

    #[test]
    fn a_whole_body_is_usable_only_before_anything_is_read() {
        // A server that ignores Range, or whose If-Range check failed, answers
        // with all of it.
        assert!(continues_from(StatusCode::OK, None, 0));
        assert!(!continues_from(StatusCode::OK, None, 4096));
    }

    #[test]
    fn an_unsatisfiable_range_does_not_splice() {
        assert!(!continues_from(
            StatusCode::RANGE_NOT_SATISFIABLE,
            None,
            4096
        ));
        assert!(!continues_from(StatusCode::RANGE_NOT_SATISFIABLE, None, 0));
    }

    #[test]
    fn content_range_start_is_the_first_byte_position() {
        assert_eq!(
            content_range_start(Some(&header("bytes 200-1000/67589"))),
            Some(200)
        );
        assert_eq!(content_range_start(Some(&header("bytes 0-0/1"))), Some(0));
        // an unsatisfied range carries no position
        assert_eq!(content_range_start(Some(&header("bytes */67589"))), None);
        // some other unit entirely
        assert_eq!(content_range_start(Some(&header("items 1-2/3"))), None);
        assert_eq!(content_range_start(None), None);
    }

    #[test]
    fn content_length_is_read_from_the_headers() {
        let mut headers = HeaderMap::new();
        assert_eq!(content_length(&headers), None);
        headers.insert(CONTENT_LENGTH, header("8192"));
        assert_eq!(content_length(&headers), Some(8192));
    }

    #[test]
    fn consecutive_failures_exhaust_the_budget() {
        let mut budget = MAX_RESUMES;
        for _ in 0..MAX_RESUMES {
            assert!(spend(&mut budget, false));
        }
        assert!(!spend(&mut budget, false));
    }

    #[test]
    fn progress_refills_the_budget() {
        // A download over a link that drops every few megabytes keeps going, so
        // long as each continuation delivers something.
        let mut budget = MAX_RESUMES;
        for _ in 0..MAX_RESUMES * 3 {
            assert!(spend(&mut budget, true));
        }
        // and once it stops moving, it still runs out
        for _ in 0..MAX_RESUMES - 1 {
            assert!(spend(&mut budget, false));
        }
        assert!(!spend(&mut budget, false));
    }

    #[test]
    fn a_server_that_serves_no_ranges_is_not_asked() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT_RANGES, header("none"));
        assert_eq!(budget_for(&headers), 0);
    }

    #[test]
    fn any_other_accept_ranges_is_worth_asking() {
        let mut headers = HeaderMap::new();
        // saying nothing is not a refusal
        assert_eq!(budget_for(&headers), MAX_RESUMES);
        headers.insert(ACCEPT_RANGES, header("bytes"));
        assert_eq!(budget_for(&headers), MAX_RESUMES);
    }
}
