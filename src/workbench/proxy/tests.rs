use super::*;
use crate::workbench::capture::{self, StreamEnd};
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::timeout;

mod native_cli;
mod reading;
mod recording;

const DEADLINE: Duration = Duration::from_secs(3);

struct Fixture {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture<F, Fut>(handler: F) -> Fixture
where
    F: Fn(Request<Body>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().fallback(handler);
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture { address, task }
}

fn client() -> Client {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(DEADLINE)
        .build()
        .unwrap()
}

async fn proxy_for(address: SocketAddr, capture: CaptureSender) -> ProxyServer {
    ProxyServer::bind_with_client(
        Upstream::parse(&format!("http://{address}/v1")).unwrap(),
        capture,
        client(),
    )
    .await
    .unwrap()
}

#[test]
fn upstream_is_fixed_and_rejects_credentials_insecure_remote_and_path_escape() {
    for url in [
        "file:///tmp/model",
        "http://model.example/v1",
        "http://localhost/v1",
        "https://user:password@model.example/v1",
        "https://model.example/v1?token=secret",
        "https://model.example/v1#fragment",
    ] {
        assert!(Upstream::parse(url).is_err());
    }
    let upstream = Upstream::parse("https://model.example/base/v1/").unwrap();
    assert_eq!(
        upstream
            .destination("/base/v1/responses", Some("api-version=synthetic"))
            .unwrap()
            .as_str(),
        "https://model.example/base/v1/responses?api-version=synthetic"
    );
    for path in [
        "/other/responses",
        "/base/v11/responses",
        "//elsewhere.example/base/v1",
        "/base/v1/../credentials",
        "/base/v1/%2e%2e/credentials",
    ] {
        assert!(upstream.destination(path, None).is_none(), "{path}");
    }
    assert!(Upstream::parse("http://[::1]:1234/v1").is_ok());
}

#[tokio::test]
async fn request_and_sse_response_stream_before_completion_with_bytes_and_headers_preserved() {
    let request_bytes = Bytes::from_static("{\"input\":\"中文请求\"}".as_bytes());
    let response_bytes =
        Bytes::from_static("data: {\"delta\":\"中文\"}\r\n\r\ndata: [DONE]\r\n\r\n".as_bytes());
    // Both cuts split a UTF-8 character; the forwarder must not decode them.
    let request_cut = 11;
    let response_cut = 17;
    let (first_request_tx, first_request_rx) = oneshot::channel();
    let (request_tail_tx, request_tail_rx) = oneshot::channel();
    let (response_tail_tx, response_tail_rx) = oneshot::channel();
    let (seen_tx, seen_rx) = oneshot::channel();
    let gates = Arc::new(Mutex::new(Some((
        first_request_tx,
        response_tail_rx,
        seen_tx,
    ))));
    let response_copy = response_bytes.clone();
    let upstream = fixture(move |request| {
        let (first_request_tx, response_tail_rx, seen_tx) = gates.lock().unwrap().take().unwrap();
        let response_copy = response_copy.clone();
        async move {
            let (parts, body) = request.into_parts();
            let mut stream = body.into_data_stream();
            let mut body = stream.next().await.unwrap().unwrap().to_vec();
            first_request_tx.send(body.clone()).unwrap();
            while let Some(bytes) = stream.next().await {
                body.extend_from_slice(&bytes.unwrap());
            }
            seen_tx.send((parts, body)).unwrap();
            let stream = async_stream::stream! {
                yield Ok::<_, std::io::Error>(response_copy.slice(..response_cut));
                response_tail_rx.await.unwrap();
                yield Ok(response_copy.slice(response_cut..));
            };
            Response::builder()
                .status(StatusCode::ACCEPTED)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header("x-request-id", "synthetic-upstream-id")
                .body(Body::from_stream(stream))
                .unwrap()
        }
    })
    .await;
    let (capture, mut observations) = capture::channel(128 * 1024, 32);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let request_copy = request_bytes.clone();
    let request_stream = async_stream::stream! {
        yield Ok::<_, std::io::Error>(request_copy.slice(..request_cut));
        request_tail_rx.await.unwrap();
        yield Ok(request_copy.slice(request_cut..));
    };
    let send = client()
        .post(format!(
            "{}/responses?api-version=synthetic",
            proxy.child_base_url()
        ))
        .header(header::AUTHORIZATION, "Bearer synthetic-credential")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-client-request-id", "synthetic-client-id")
        .body(reqwest::Body::wrap_stream(request_stream))
        .send();
    let pending = tokio::spawn(send);
    assert_eq!(
        timeout(DEADLINE, first_request_rx).await.unwrap().unwrap(),
        request_bytes[..request_cut]
    );
    request_tail_tx.send(()).unwrap();
    let mut response = pending.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(response.headers()["x-request-id"], "synthetic-upstream-id");
    let first = response.chunk().await.unwrap().unwrap();
    assert_eq!(first, response_bytes[..response_cut]);
    response_tail_tx.send(()).unwrap();
    let mut received = first.to_vec();
    while let Some(bytes) = response.chunk().await.unwrap() {
        received.extend_from_slice(&bytes);
    }
    assert_eq!(received, response_bytes);
    let (parts, body) = seen_rx.await.unwrap();
    assert_eq!(parts.method, Method::POST);
    assert_eq!(parts.uri, "/v1/responses?api-version=synthetic");
    assert_eq!(parts.headers[header::HOST], upstream.address.to_string());
    assert_eq!(
        parts.headers[header::AUTHORIZATION],
        "Bearer synthetic-credential"
    );
    assert_eq!(parts.headers["x-client-request-id"], "synthetic-client-id");
    assert_eq!(body, request_bytes);
    let mut request_capture = Vec::new();
    let mut response_capture = Vec::new();
    let mut ends = 0;
    while ends < 2 {
        let observation = timeout(DEADLINE, observations.recv())
            .await
            .unwrap()
            .unwrap();
        ends += usize::from(observation.end == Some(StreamEnd::Complete));
        match observation.source.direction {
            Direction::Request => request_capture.extend_from_slice(&observation.bytes),
            Direction::Response => response_capture.extend_from_slice(&observation.bytes),
        }
    }
    assert_eq!(request_capture, request_bytes);
    assert_eq!(response_capture, response_bytes);
    assert_eq!(capture.stats().dropped_chunks, 0);
    assert_eq!(capture.stats().interrupted_streams, 0);
    assert_eq!(capture.stats().retained_bytes, 0);
}

#[tokio::test]
async fn compression_error_status_and_redirect_are_not_rewritten_or_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    // gzip of "synthetic", generated ahead of time. No decompression in proxy.
    let compressed = Bytes::from_static(&[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 43, 174, 204, 43, 201, 72, 45, 201, 76, 6, 0, 87, 2,
        196, 65, 9, 0, 0, 0,
    ]);
    let expected = compressed.clone();
    let upstream = fixture(move |request| {
        let count = count.clone();
        let compressed = compressed.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if request.uri().path().ends_with("redirect") {
                Response::builder()
                    .status(StatusCode::TEMPORARY_REDIRECT)
                    .header(header::LOCATION, "https://unrelated.invalid/credentials")
                    .body(Body::from("redirect body"))
                    .unwrap()
            } else {
                Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header(header::CONTENT_ENCODING, "gzip")
                    .header(header::RETRY_AFTER, "60")
                    .body(Body::from(compressed))
                    .unwrap()
            }
        }
    })
    .await;
    let (capture, _observations) = capture::channel(1024, 32);
    let proxy = proxy_for(upstream.address, capture).await;
    let response = client()
        .get(format!("{}/error", proxy.child_base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    assert_eq!(response.headers()[header::RETRY_AFTER], "60");
    assert_eq!(response.bytes().await.unwrap(), expected);
    let response = client()
        .get(format!("{}/redirect", proxy.child_base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://unrelated.invalid/credentials"
    );
    assert_eq!(response.text().await.unwrap(), "redirect body");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn browser_bad_host_missing_capability_and_wrong_methods_never_reach_upstream() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let upstream = fixture(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        async { Body::empty().into_response() }
    })
    .await;
    let (capture, _observations) = capture::channel(1024, 16);
    let proxy = proxy_for(upstream.address, capture).await;
    let url = format!("{}/responses", proxy.child_base_url());
    for (builder, status) in [
        (
            client()
                .post(&url)
                .header(header::ORIGIN, "http://evil.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (
            client().post(&url).header("sec-fetch-site", "same-origin"),
            StatusCode::FORBIDDEN,
        ),
        (
            client().post(&url).header(header::HOST, "evil.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (client().put(&url), StatusCode::METHOD_NOT_ALLOWED),
        (
            client().get(format!("http://{}/v1/responses", proxy.address())),
            StatusCode::NOT_FOUND,
        ),
        (
            client().post(&url).header(header::UPGRADE, "websocket"),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(builder.send().await.unwrap().status(), status);
    }
    for target in ["/other/responses", "/v1/%2e%2e/secrets", "/v11/responses"] {
        let path = format!("/{}{}", proxy.state.capability, target);
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            proxy.address()
        );
        let mut connection = TcpStream::connect(proxy.address()).await.unwrap();
        connection.write_all(head.as_bytes()).await.unwrap();
        assert!(read_head(&mut connection).await.starts_with("HTTP/1.1 404"));
    }
    for (request_line, expected) in [
        (
            "GET https://elsewhere.invalid/v1/responses HTTP/1.1",
            "HTTP/1.1 403",
        ),
        ("CONNECT elsewhere.invalid:443 HTTP/1.1", "HTTP/1.1 405"),
    ] {
        let head = format!(
            "{request_line}\r\nHost: {}\r\nConnection: close\r\n\r\n",
            proxy.address()
        );
        let mut connection = TcpStream::connect(proxy.address()).await.unwrap();
        connection.write_all(head.as_bytes()).await.unwrap();
        assert!(read_head(&mut connection).await.starts_with(expected));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stopped_observer_for_five_seconds_cannot_backpressure_forwarding() {
    let payload = Bytes::from(vec![b'x'; 512 * 1024]);
    let upstream = fixture(move |_| {
        let payload = payload.clone();
        async move { Body::from(payload).into_response() }
    })
    .await;
    let (capture, observations) = capture::channel(1024, 2);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let stalled = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        observations
    });
    let bytes = client()
        .get(format!("{}/responses", proxy.child_base_url()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.len(), 512 * 1024);
    assert!(
        !stalled.is_finished(),
        "forwarding waited for stalled observer"
    );
    assert!(capture.stats().retained_bytes <= 1024);
    assert!(capture.stats().dropped_chunks > 0);
    let observations = stalled.await.unwrap();
    drop(observations);
    assert_eq!(capture.stats().retained_bytes, 0);
    let bytes = client()
        .get(format!("{}/responses", proxy.child_base_url()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.len(), 512 * 1024);
    assert_eq!(capture.stats().retained_bytes, 0);
}

async fn read_head(connection: &mut TcpStream) -> String {
    timeout(DEADLINE, async {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(connection.read_u8().await.unwrap());
            assert!(head.len() < 16 * 1024);
        }
        String::from_utf8(head).unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn websocket_tunnel_preserves_raw_fragment_mask_compression_control_and_close_bytes() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    // Two masked fragments with an intervening ping. The tunnel must leave all
    // framing bytes intact, even if a future decoder cannot read an extension.
    let client_frames: Vec<u8> = vec![
        0x01,
        0x82,
        1,
        2,
        3,
        4,
        b'h' ^ 1,
        b'e' ^ 2,
        0x89,
        0x81,
        5,
        6,
        7,
        8,
        b'!' ^ 5,
        0x80,
        0x83,
        1,
        2,
        3,
        4,
        b'l' ^ 1,
        b'l' ^ 2,
        b'o' ^ 3,
        0x82,
        0x82,
        9,
        8,
        7,
        6,
        0,
        0xff,
        0x88,
        0x82,
        1,
        2,
        3,
        4,
        0x03 ^ 1,
        0xe8 ^ 2,
    ];
    // An RSV1 compressed text message (raw deflate of "Hi"), binary, pong, close.
    let server_frames: Vec<u8> = vec![
        0xc1, 4, 0xf2, 0xc8, 4, 0, 0x82, 2, 0, 0xff, 0x8a, 1, b'!', 0x88, 2, 0x03, 0xe8,
    ];
    let expected_client = client_frames.clone();
    let outbound = server_frames.clone();
    let server = tokio::spawn(async move {
        let (mut connection, _) = listener.accept().await.unwrap();
        let head = read_head(&mut connection).await;
        assert!(head.starts_with("GET /v1/responses HTTP/1.1"));
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer synthetic-credential")
        );
        assert!(head.contains("permessage-deflate"));
        connection.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n").await.unwrap();
        let mut received = vec![0; expected_client.len()];
        timeout(DEADLINE, connection.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, expected_client);
        for part in outbound.chunks(3) {
            connection.write_all(part).await.unwrap();
        }
        connection.shutdown().await.unwrap();
    });
    let (capture, mut observations) = capture::channel(16 * 1024, 64);
    let proxy = proxy_for(address, capture.clone()).await;
    let path = Url::parse(&format!("{}/responses", proxy.child_base_url()))
        .unwrap()
        .path()
        .to_owned();
    let mut connection = TcpStream::connect(proxy.address()).await.unwrap();
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Extensions: permessage-deflate\r\nAuthorization: Bearer synthetic-credential\r\n\r\n",
        proxy.address()
    );
    connection.write_all(head.as_bytes()).await.unwrap();
    let head = read_head(&mut connection).await;
    assert!(head.starts_with("HTTP/1.1 101"));
    assert!(head.contains("permessage-deflate"));
    for part in client_frames.chunks(2) {
        connection.write_all(part).await.unwrap();
    }
    connection.shutdown().await.unwrap();
    let mut received = Vec::new();
    timeout(DEADLINE, connection.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, server_frames);
    server.await.unwrap();
    let mut captured_request = Vec::new();
    let mut captured_response = Vec::new();
    let mut ends = 0;
    while ends < 2 {
        let observation = timeout(DEADLINE, observations.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observation.source.transport, Transport::WebSocket);
        ends += usize::from(observation.end == Some(StreamEnd::Complete));
        match observation.source.direction {
            Direction::Request => captured_request.extend_from_slice(&observation.bytes),
            Direction::Response => captured_response.extend_from_slice(&observation.bytes),
        }
    }
    assert_eq!(captured_request, client_frames);
    assert_eq!(captured_response, server_frames);
    assert_eq!(capture.stats().interrupted_streams, 0);
}

#[tokio::test]
async fn truncated_upstream_body_is_an_error_and_an_interrupted_observation() {
    let (release_tx, release_rx) = oneshot::channel();
    let gate = Arc::new(Mutex::new(Some(release_rx)));
    let upstream = fixture(move |_| {
        let release_rx = gate.lock().unwrap().take().unwrap();
        async move {
            let stream = async_stream::stream! {
                yield Ok(Bytes::from_static(b"data: {\"partial\":true}\n\n"));
                release_rx.await.unwrap();
                yield Err(std::io::Error::other("synthetic body failure"));
            };
            Body::from_stream(stream).into_response()
        }
    })
    .await;
    let (capture, mut observations) = capture::channel(1024, 16);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    let mut response = client()
        .get(format!("{}/responses", proxy.child_base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.chunk().await.unwrap().unwrap(),
        "data: {\"partial\":true}\n\n"
    );
    release_tx.send(()).unwrap();
    assert!(response.chunk().await.is_err());
    loop {
        let observation = timeout(DEADLINE, observations.recv())
            .await
            .unwrap()
            .unwrap();
        if observation.source.direction == Direction::Response && observation.end.is_some() {
            assert_eq!(observation.end, Some(StreamEnd::Interrupted));
            break;
        }
    }
    assert_eq!(capture.stats().interrupted_streams, 1);
}

#[tokio::test]
async fn empty_and_fixed_length_bodies_finish_without_false_capture_gaps() {
    let upstream = fixture(|request| async {
        let body = axum::body::to_bytes(request.into_body(), 1024)
            .await
            .unwrap();
        Response::builder()
            .header(header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .unwrap()
    })
    .await;
    let (capture, mut observations) = capture::channel(1024, 16);
    let proxy = proxy_for(upstream.address, capture.clone()).await;
    for bytes in ["", "fixed-length-中文"] {
        let response = client()
            .post(format!("{}/responses", proxy.child_base_url()))
            .body(bytes)
            .send()
            .await
            .unwrap();
        assert_eq!(response.bytes().await.unwrap(), bytes.as_bytes());
        let mut ends = 0;
        while ends < 2 {
            let observation = timeout(DEADLINE, observations.recv())
                .await
                .unwrap()
                .unwrap();
            if let Some(end) = observation.end {
                assert_eq!(end, StreamEnd::Complete);
                ends += 1;
            }
        }
    }
    assert_eq!(capture.stats().interrupted_streams, 0);
}

#[tokio::test]
async fn active_forwarding_has_a_limit_and_disconnect_returns_capacity() {
    let upstream = fixture(|_| async {
        let stream = async_stream::stream! {
            yield Ok::<_, std::io::Error>(Bytes::from_static(b"first"));
            std::future::pending::<()>().await;
        };
        Body::from_stream(stream).into_response()
    })
    .await;
    let (capture, _observations) = capture::channel(1024, 16);
    let proxy = proxy_for(upstream.address, capture).await;
    let client = client();
    let url = format!("{}/responses", proxy.child_base_url());
    let mut responses = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        let mut response = client.get(&url).send().await.unwrap();
        assert_eq!(response.chunk().await.unwrap().unwrap(), "first");
        responses.push(response);
    }
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    responses.clear();
    timeout(DEADLINE, async {
        while proxy.state.slots.available_permits() != MAX_CONNECTIONS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn disconnect_and_run_shutdown_release_streaming_connections() {
    struct SignalOnDrop(Option<oneshot::Sender<()>>);
    impl Drop for SignalOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    for stop_run in [false, true] {
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let signal = Arc::new(Mutex::new(Some(dropped_tx)));
        let upstream = fixture(move |_| {
            let guard = SignalOnDrop(signal.lock().unwrap().take());
            async move {
                let stream = async_stream::stream! {
                    let _guard = guard;
                    yield Ok::<_, std::io::Error>(Bytes::from_static(b"data: first\n\n"));
                    std::future::pending::<()>().await;
                };
                Body::from_stream(stream).into_response()
            }
        })
        .await;
        let (capture, _observations) = capture::channel(1024, 16);
        let proxy = proxy_for(upstream.address, capture.clone()).await;
        let mut response = client()
            .get(format!("{}/responses", proxy.child_base_url()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.chunk().await.unwrap().unwrap(), "data: first\n\n");
        if stop_run {
            drop(proxy);
        } else {
            drop(response);
        }
        timeout(DEADLINE, dropped_rx).await.unwrap().unwrap();
        assert!(capture.stats().interrupted_streams > 0);
    }
}
