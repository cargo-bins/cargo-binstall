//! End-to-end tests of the opportunistic HTTP/3 support against local
//! servers: an HTTPS/1.1 server over TCP and an HTTP/3 server over UDP on the
//! same port, each answering with a body naming its transport.

#![cfg(all(reqwest_unstable, feature = "http3"))]

use std::{
    net::{Ipv4Addr, SocketAddr},
    num::{NonZeroU16, NonZeroU64},
    sync::Arc,
};

use binstalk_downloader::remote::{Certificate, Client, DnsResolver, Url};
use bytes::Bytes;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

/// What a test server answers for a path.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: &'static str,
}

type Handler = Arc<dyn Fn(&str) -> Reply + Send + Sync>;

struct TestCert {
    pem: String,
    der: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

fn test_cert() -> TestCert {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(["127.0.0.1".to_owned()]).unwrap();
    TestCert {
        pem: cert.pem(),
        der: cert.der().clone(),
        key: PrivatePkcs8KeyDer::from(signing_key.serialize_der()),
    }
}

fn server_config(cert: &TestCert, alpn: &[u8]) -> ServerConfig {
    let mut config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der.clone()],
        PrivateKeyDer::Pkcs8(cert.key.clone_key()),
    )
    .unwrap();
    config.alpn_protocols = vec![alpn.to_vec()];
    config
}

/// Serve HTTP/1.1 over TLS, one request per connection.
fn spawn_tcp(listener: TcpListener, cert: &TestCert, handler: Handler) {
    let acceptor = TlsAcceptor::from(Arc::new(server_config(cert, b"http/1.1")));

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (acceptor, handler) = (acceptor.clone(), handler.clone());

            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };

                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut buf = [0; 1024];
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split(' ').nth(1).unwrap();

                let Reply {
                    status,
                    headers,
                    body,
                } = handler(path);
                let mut response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    response += &format!("{name}: {value}\r\n");
                }
                response += "\r\n";
                response += body;

                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.ok();
            });
        }
    });
}

/// Serve HTTP/3.
fn spawn_h3(endpoint: quinn::Endpoint, handler: Handler) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let handler = handler.clone();

            tokio::spawn(async move {
                let Ok(conn) = incoming.await else {
                    return;
                };
                let mut conn =
                    h3::server::Connection::<_, Bytes>::new(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();

                while let Ok(Some(resolver)) = conn.accept().await {
                    let handler = handler.clone();

                    tokio::spawn(async move {
                        let (request, mut stream) = resolver.resolve_request().await.unwrap();
                        // Drain the request so that dropping the stream does not
                        // send STOP_SENDING, which reqwest mishandles
                        // (https://github.com/seanmonstar/reqwest/issues/3147).
                        while stream.recv_data().await.unwrap().is_some() {}

                        let Reply {
                            status,
                            headers,
                            body,
                        } = handler(request.uri().path());
                        let mut response = http::Response::builder().status(status);
                        for (name, value) in headers {
                            response = response.header(name, value);
                        }

                        stream
                            .send_response(response.body(()).unwrap())
                            .await
                            .unwrap();
                        stream
                            .send_data(Bytes::from_static(body.as_bytes()))
                            .await
                            .unwrap();
                        stream.finish().await.unwrap();
                    });
                }
            });
        }
    });
}

struct Origin {
    addr: SocketAddr,
    /// Kept alive so that the UDP port stays taken even without an HTTP/3
    /// server, making sure nothing else answers there.
    _udp: Option<std::net::UdpSocket>,
}

impl Origin {
    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("https://{}{path}", self.addr)).unwrap()
    }
}

/// Start an origin with a TCP server, plus an HTTP/3 server on the same port
/// if `h3` is set; otherwise the UDP port is bound but silently drops
/// everything, like a firewall would.
async fn spawn_origin(
    cert: &TestCert,
    tcp: impl Fn(&str, u16) -> Reply + Send + Sync + 'static,
    h3: Option<Handler>,
) -> Origin {
    loop {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();

        let Ok(udp) = std::net::UdpSocket::bind(addr) else {
            // Someone else already has the UDP port, try another one.
            continue;
        };

        spawn_tcp(listener, cert, Arc::new(move |path| tcp(path, port)));

        let udp = match h3 {
            Some(h3) => {
                let config = quinn::ServerConfig::with_crypto(Arc::new(
                    quinn::crypto::rustls::QuicServerConfig::try_from(server_config(cert, b"h3"))
                        .unwrap(),
                ));
                let endpoint = quinn::Endpoint::new(
                    Default::default(),
                    Some(config),
                    udp,
                    Arc::new(quinn::TokioRuntime),
                )
                .unwrap();
                spawn_h3(endpoint, h3);
                None
            }
            None => Some(udp),
        };

        break Origin { addr, _udp: udp };
    }
}

fn client(cert: &TestCert) -> Client {
    Client::new(
        "binstalk-downloader-http3-test",
        None,
        false,
        DnsResolver::System,
        NonZeroU16::new(10).unwrap(),
        NonZeroU64::new(10).unwrap(),
        [Certificate::from_pem(&cert.pem).unwrap()],
    )
    .unwrap()
}

async fn get(client: &Client, url: Url) -> String {
    let body = client
        .get(url)
        .send(true)
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

fn ok(body: &'static str, port: u16) -> Reply {
    Reply {
        status: 200,
        headers: vec![("alt-svc", format!(r#"h3=":{port}"; ma=60"#))],
        body,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrades_to_h3_after_alt_svc() {
    let cert = test_cert();
    let origin = spawn_origin(
        &cert,
        |_, port| ok("tcp", port),
        Some(Arc::new(|_| Reply {
            status: 200,
            headers: vec![],
            body: "h3",
        })),
    )
    .await;
    let client = client(&cert);

    assert_eq!(get(&client, origin.url("/")).await, "tcp");
    assert_eq!(get(&client, origin.url("/")).await, "h3");
    assert_eq!(get(&client, origin.url("/other")).await, "h3");
}

#[tokio::test(flavor = "multi_thread")]
async fn falls_back_to_tcp_when_udp_is_dropped() {
    let cert = test_cert();
    let origin = spawn_origin(&cert, |_, port| ok("tcp", port), None).await;
    let client = client(&cert);

    assert_eq!(get(&client, origin.url("/")).await, "tcp");
    // HTTP/3 is tried, gets no answer and loses the race against TCP.
    assert_eq!(get(&client, origin.url("/")).await, "tcp");
    assert_eq!(get(&client, origin.url("/")).await, "tcp");
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_to_origin_without_h3_goes_over_tcp() {
    let cert = test_cert();
    let target = spawn_origin(
        &cert,
        |_, _| Reply {
            status: 200,
            headers: vec![],
            body: "target over tcp",
        },
        None,
    )
    .await;
    let location = target.url("/").to_string();

    let redirect = {
        let location = location.clone();
        move |path: &str, port: Option<u16>| {
            let mut headers: Vec<_> = port
                .map(|port| ("alt-svc", format!(r#"h3=":{port}"; ma=60"#)))
                .into_iter()
                .collect();
            if path == "/redirect" {
                headers.push(("location", location.clone()));
                Reply {
                    status: 302,
                    headers,
                    body: "",
                }
            } else {
                Reply {
                    status: 200,
                    headers,
                    body: if port.is_some() {
                        "origin over tcp"
                    } else {
                        "origin over h3"
                    },
                }
            }
        }
    };
    let h3_redirect = redirect.clone();
    let origin = spawn_origin(
        &cert,
        move |path, port| redirect(path, Some(port)),
        Some(Arc::new(move |path| h3_redirect(path, None))),
    )
    .await;
    let client = client(&cert);

    assert_eq!(get(&client, origin.url("/")).await, "origin over tcp");
    assert_eq!(get(&client, origin.url("/")).await, "origin over h3");
    assert_eq!(
        get(&client, origin.url("/redirect")).await,
        "target over tcp"
    );
    // The origin's HTTP/3 itself is fine and still used.
    assert_eq!(get(&client, origin.url("/")).await, "origin over h3");
}

#[tokio::test(flavor = "multi_thread")]
async fn redirect_from_tcp_origin_to_h3_origin_uses_h3() {
    let cert = test_cert();
    let cdn = spawn_origin(
        &cert,
        |_, port| ok("cdn over tcp", port),
        Some(Arc::new(|_| Reply {
            status: 200,
            headers: vec![],
            body: "cdn over h3",
        })),
    )
    .await;
    let location = cdn.url("/asset").to_string();
    let origin = spawn_origin(
        &cert,
        move |_, _| Reply {
            status: 302,
            headers: vec![("location", location.clone())],
            body: "",
        },
        None,
    )
    .await;
    let client = client(&cert);

    assert_eq!(get(&client, cdn.url("/")).await, "cdn over tcp");
    assert_eq!(get(&client, origin.url("/")).await, "cdn over h3");
}
