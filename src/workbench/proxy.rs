//! Fixed-upstream model proxy. Forwarding never waits for observation consumers.
//!
//! HTTP bodies remain streamed. WebSockets use an upgraded byte tunnel, leaving
//! masking, fragments, extension frames, ping/pong and close frames unchanged.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, Method, Request, StatusCode, Version, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use hyper_util::rt::TokioIo;
use reqwest::{Client, Url};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::capture::{CaptureSender, CaptureStream, Direction, Source, Transport};

const MAX_CONNECTIONS: usize = 64;
const COPY_BUFFER: usize = 16 * 1024;

// Never derive Debug: the local routing capability and upstream may be private.
#[derive(Clone)]
pub struct Upstream {
    base: Url,
}

impl Upstream {
    pub fn parse(value: &str) -> Result<Self> {
        let base = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid upstream URL"))?;
        let is_loopback = base
            .host_str()
            .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if !matches!(base.scheme(), "http" | "https")
            || (base.scheme() == "http" && !is_loopback)
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!(
                "upstream requires HTTPS (or loopback HTTP), without credentials, query or fragment"
            );
        }
        Ok(Self { base })
    }

    pub fn base_path(&self) -> &str {
        self.base.path().trim_end_matches('/')
    }

    fn destination(&self, local_path: &str, query: Option<&str>) -> Option<Url> {
        let base_path = self.base_path();
        if local_path != base_path
            && !local_path
                .strip_prefix(base_path)
                .is_some_and(|suffix| suffix.starts_with('/'))
        {
            return None;
        }
        let mut url = self.base.clone();
        url.set_path(local_path);
        url.set_query(query);
        // URL parsing normalizes dot segments. Reject normalization that escapes
        // the configured base; never let user input replace the target authority.
        if url.path() != local_path {
            return None;
        }
        Some(url)
    }
}

struct ProxyState {
    upstream: Upstream,
    client: Client,
    capture: CaptureSender,
    capability: String,
    authority: String,
    slots: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
}

pub struct ProxyServer {
    address: SocketAddr,
    state: Arc<ProxyState>,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ProxyServer {
    pub async fn bind(upstream: Upstream, capture: CaptureSender) -> Result<Self> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            // Feature unification must not silently enable decompression.
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .build()
            .context("build model forwarding client")?;
        Self::bind_with_client(upstream, capture, client).await
    }

    async fn bind_with_client(
        upstream: Upstream,
        capture: CaptureSender,
        client: Client,
    ) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("bind loopback model proxy")?;
        let address = listener.local_addr()?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let state = Arc::new(ProxyState {
            upstream,
            client,
            capture,
            capability: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            authority: address.to_string(),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            shutdown: shutdown_rx.clone(),
        });
        let app = Router::new().fallback(forward).with_state(state.clone());
        let task = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.changed().await;
                })
                .await;
        });
        Ok(Self {
            address,
            state,
            shutdown,
            task,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    #[cfg(test)]
    pub(crate) fn abort_for_test(&self) {
        self.task.abort();
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Sensitive child-process configuration; must not be logged or shown in UI.
    pub fn child_base_url(&self) -> String {
        format!(
            "http://{}/{}{}",
            self.address,
            self.state.capability,
            self.state.upstream.base_path()
        )
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

fn diagnostic(status: StatusCode, code: &'static str, request_id: Uuid) -> Response {
    (
        status,
        axum::Json(serde_json::json!({
            "error": { "source": "model_proxy", "code": code, "requestId": request_id }
        })),
    )
        .into_response()
}

fn strip_hop_headers(headers: &mut HeaderMap, upgrade: bool) {
    let nominated: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|value| HeaderName::from_bytes(value.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        if !(upgrade && name == header::UPGRADE) {
            headers.remove(name);
        }
    }
    for name in [
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    if upgrade {
        headers.insert(header::CONNECTION, "upgrade".parse().unwrap());
    } else {
        headers.remove(header::CONNECTION);
        headers.remove(header::UPGRADE);
    }
}

fn source(
    request_id: Uuid,
    direction: Direction,
    transport: Transport,
    headers: &HeaderMap,
) -> Source {
    let hint = |name: HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.len() <= 128)
            .unwrap_or("")
            .to_owned()
    };
    Source {
        request_id,
        direction,
        transport,
        content_type: hint(header::CONTENT_TYPE),
        content_encoding: hint(header::CONTENT_ENCODING),
    }
}

async fn forward(State(state): State<Arc<ProxyState>>, mut request: Request<Body>) -> Response {
    let request_id = Uuid::new_v4();
    if request.headers().contains_key(header::ORIGIN)
        || request.headers().contains_key("sec-fetch-site")
        || request
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            != Some(state.authority.as_str())
        || request.uri().scheme().is_some()
    {
        return diagnostic(StatusCode::FORBIDDEN, "browser_or_invalid_host", request_id);
    }
    if !matches!(request.method(), &Method::GET | &Method::POST) {
        return diagnostic(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            request_id,
        );
    }
    let prefix = format!("/{}", state.capability);
    let Some(path) = request.uri().path().strip_prefix(&prefix) else {
        return diagnostic(StatusCode::NOT_FOUND, "invalid_route", request_id);
    };
    if !path.starts_with('/') {
        return diagnostic(StatusCode::NOT_FOUND, "invalid_route", request_id);
    }
    let Some(destination) = state.upstream.destination(path, request.uri().query()) else {
        return diagnostic(StatusCode::NOT_FOUND, "invalid_route", request_id);
    };
    let Ok(permit) = state.slots.clone().try_acquire_owned() else {
        return diagnostic(
            StatusCode::SERVICE_UNAVAILABLE,
            "connection_limit",
            request_id,
        );
    };
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if websocket && request.method() != Method::GET {
        return diagnostic(StatusCode::BAD_REQUEST, "invalid_upgrade", request_id);
    }
    let downstream_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    let transport = if websocket {
        Transport::WebSocket
    } else {
        Transport::Http
    };
    let request_source = source(request_id, Direction::Request, transport, &headers);
    headers.remove(header::HOST);
    strip_hop_headers(&mut headers, websocket);
    let mut outgoing = state
        .client
        .request(parts.method, destination)
        .headers(headers);
    if websocket {
        outgoing = outgoing.version(Version::HTTP_11);
    } else {
        let mut request_capture = state.capture.stream(request_source);
        if body.is_end_stream() {
            request_capture.finish();
        } else {
            let body = async_stream::stream! {
                let mut body = body;
                while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
                    match frame {
                        Ok(frame) => {
                            if let Ok(bytes) = frame.into_data() {
                                request_capture.offer(bytes.clone(), Instant::now());
                                // A known Content-Length can let the HTTP client
                                // stop polling without requesting a final None.
                                if body.is_end_stream() { request_capture.finish(); }
                                yield Ok::<_, axum::Error>(bytes);
                            }
                        }
                        Err(error) => { yield Err(error); return; }
                    }
                }
                request_capture.finish();
            };
            outgoing = outgoing.body(reqwest::Body::wrap_stream(body));
        }
    }
    let mut shutdown = state.shutdown.clone();
    let upstream = tokio::select! {
        value = outgoing.send() => value,
        _ = shutdown.changed() => return diagnostic(StatusCode::SERVICE_UNAVAILABLE, "run_stopped", request_id),
    };
    let Ok(upstream) = upstream else {
        // reqwest errors contain URLs. Neither credentials nor capabilities may
        // appear in errors sent to a client or general-purpose logs.
        return diagnostic(StatusCode::BAD_GATEWAY, "upstream_failed", request_id);
    };
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    let response_transport = if status == StatusCode::SWITCHING_PROTOCOLS {
        transport
    } else {
        Transport::Http
    };
    let response_source = source(
        request_id,
        Direction::Response,
        response_transport,
        &headers,
    );
    let mut response_capture = state.capture.stream(response_source);
    if status == StatusCode::SWITCHING_PROTOCOLS {
        let Some(downstream_upgrade) = downstream_upgrade else {
            return diagnostic(StatusCode::BAD_GATEWAY, "unexpected_upgrade", request_id);
        };
        strip_hop_headers(&mut headers, true);
        let Ok(upstream) = upstream.upgrade().await else {
            return diagnostic(StatusCode::BAD_GATEWAY, "upgrade_failed", request_id);
        };
        let mut request_capture = state.capture.stream(Source {
            request_id,
            direction: Direction::Request,
            transport: Transport::WebSocket,
            content_type: String::new(),
            content_encoding: String::new(),
        });
        tokio::spawn(async move {
            let _permit = permit;
            let connection = async {
                let downstream = downstream_upgrade.await?;
                let (mut client_read, mut client_write) =
                    tokio::io::split(TokioIo::new(downstream));
                let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
                tokio::try_join!(
                    tunnel(&mut client_read, &mut upstream_write, &mut request_capture),
                    tunnel(&mut upstream_read, &mut client_write, &mut response_capture),
                )?;
                Ok::<(), anyhow::Error>(())
            };
            tokio::select! {
                _ = connection => {},
                _ = shutdown.changed() => {},
            }
        });
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return response;
    }
    strip_hop_headers(&mut headers, false);
    let mut remaining = upstream.content_length();
    if remaining == Some(0) {
        response_capture.finish();
    }
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut chunks = upstream.bytes_stream();
        loop {
            let next = tokio::select! {
                value = chunks.next() => value,
                _ = shutdown.changed() => return,
            };
            match next {
                Some(Ok(bytes)) => {
                    response_capture.offer(bytes.clone(), Instant::now());
                    if let Some(remaining) = remaining.as_mut() {
                        *remaining = remaining.saturating_sub(bytes.len() as u64);
                        if *remaining == 0 { response_capture.finish(); }
                    }
                    yield Ok::<_, std::io::Error>(bytes);
                }
                Some(Err(_)) => {
                    yield Err(std::io::Error::other("model_proxy: upstream_body_failed"));
                    return;
                }
                None => { response_capture.finish(); return; }
            }
        }
    };
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

async fn tunnel<R, W>(
    reader: &mut R,
    writer: &mut W,
    capture: &mut CaptureStream,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = [0u8; COPY_BUFFER];
    loop {
        let size = reader.read(&mut buffer).await?;
        if size == 0 {
            capture.finish();
            writer.shutdown().await?;
            return Ok(());
        }
        let received_at = Instant::now();
        writer.write_all(&buffer[..size]).await?;
        capture.offer(Bytes::copy_from_slice(&buffer[..size]), received_at);
    }
}

#[cfg(test)]
mod tests;
