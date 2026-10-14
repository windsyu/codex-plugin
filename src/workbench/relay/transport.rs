use super::{Shared, storage::Saved};
use crate::workbench::lease_clock::{Clock, Lease};
use crate::workbench::remote_access::IngressHandle;
use anyhow::{Result, bail};
use codex_relay_protocol::{
    self as protocol, Data, Hello, Open, StreamKind, ToGateway, ToRelay, Welcome,
};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    task::JoinSet,
    time::{Instant, timeout},
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use uuid::Uuid;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const CONNECT: Duration = Duration::from_secs(10);
const WRITE: Duration = Duration::from_secs(2);

struct Backoff {
    ceiling_ms: u64,
    connected_at: Option<Instant>,
}
impl Default for Backoff {
    fn default() -> Self {
        Self {
            ceiling_ms: 1_000,
            connected_at: None,
        }
    }
}
impl Backoff {
    fn connected(&mut self, now: Instant) {
        self.connected_at = Some(now);
    }
    fn control_received(&mut self, now: Instant) {
        // A welcome followed by an immediate disconnect is not recovery. Require
        // live control traffic after a complete renewal interval before resetting.
        if self.connected_at.is_some_and(|start| {
            now.duration_since(start) >= Duration::from_secs(protocol::RENEW_SECONDS)
        }) {
            self.ceiling_ms = 1_000;
            self.connected_at = None;
        }
    }
    fn next_delay(&mut self) -> Duration {
        self.connected_at = None;
        let delay =
            Duration::from_millis(rand::random_range(self.ceiling_ms / 2..=self.ceiling_ms));
        self.ceiling_ms = (self.ceiling_ms * 2).min(30_000);
        delay
    }
}

pub(super) fn server_url(value: &str, insecure: bool) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("invalid relay URL"))?;
    let loopback = url.host_str().is_some_and(|host| {
        localhost_name(host)
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if (url.scheme() != "https" && !(insecure && loopback && url.scheme() == "http"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!("relay requires an HTTPS origin; insecure loopback requires explicit opt-in");
    }
    Ok(url)
}
fn localhost_name(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost")
}
pub(super) fn client(base: &str) -> Result<Client> {
    let url = Url::parse(base)?;
    let mut builder = Client::builder();
    if url.scheme() == "http"
        && let Some(host) = url.host_str().filter(|host| localhost_name(host))
    {
        builder = builder.resolve(
            host,
            std::net::SocketAddr::from(([127, 0, 0, 1], url.port_or_known_default().unwrap_or(80))),
        );
    }
    Ok(builder
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .timeout(CONNECT)
        .build()?)
}
fn local_client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .build()?)
}
fn ws_url(base: &str, path: &str) -> Result<String> {
    let mut url = Url::parse(base)?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid socket origin"))?;
    let url = url.join(path)?;
    Ok(url.into())
}
async fn connect(url: String, credential: &str, max_message: usize) -> Result<Socket> {
    let parsed = Url::parse(&url)?;
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {credential}").parse()?);
    let config = WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message))
        .max_write_buffer_size(2 * max_message)
        .write_buffer_size(0);
    if parsed.scheme() == "ws" && parsed.host_str().is_some_and(localhost_name) {
        // RFC 6761 names never use ambient DNS for insecure development transport.
        let socket = timeout(
            CONNECT,
            TcpStream::connect((
                std::net::Ipv4Addr::LOCALHOST,
                parsed.port_or_known_default().unwrap_or(80),
            )),
        )
        .await??;
        socket.set_nodelay(true)?;
        return Ok(timeout(
            CONNECT,
            tokio_tungstenite::client_async_with_config(
                request,
                MaybeTlsStream::Plain(socket),
                Some(config),
            ),
        )
        .await??
        .0);
    }
    Ok(timeout(
        CONNECT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await??
    .0)
}
async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    timeout(WRITE, socket.send(message)).await??;
    Ok(())
}
async fn json(socket: &mut Socket, value: &impl serde::Serialize) -> Result<()> {
    send(socket, Message::Text(serde_json::to_string(value)?.into())).await
}
struct Fence(IngressHandle);
impl Drop for Fence {
    fn drop(&mut self) {
        self.0.disconnect();
    }
}
/// Returns true only for a permanently rejected credential. Transient network
/// failures remain reconnectable and never replay previous input.
pub(super) async fn run(
    saved: Saved,
    instance: Uuid,
    ingress: IngressHandle,
    snapshot: Shared,
) -> bool {
    let mut backoff = Backoff::default();
    loop {
        {
            let mut state = snapshot.lock().unwrap();
            state.state = "connecting".into();
            state.error = None;
        }
        let result = connection_attempt(&saved, instance, &ingress, &snapshot, &mut backoff).await;
        ingress.disconnect();
        if result.as_ref().err().is_some_and(authentication_rejected) {
            return true;
        }
        {
            let mut state = snapshot.lock().unwrap();
            state.state = "offline".into();
            state.error = Some("relay_disconnected".into());
        }
        tokio::time::sleep(backoff.next_delay()).await;
    }
}
fn authentication_rejected(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
        Some(tokio_tungstenite::tungstenite::Error::Http(response))
        if matches!(response.status().as_u16(), 401 | 403))
}
#[cfg(test)]
async fn connection(
    saved: &Saved,
    instance: Uuid,
    ingress: &IngressHandle,
    snapshot: &Shared,
) -> Result<()> {
    connection_attempt(saved, instance, ingress, snapshot, &mut Backoff::default()).await
}
async fn connection_attempt(
    saved: &Saved,
    instance: Uuid,
    ingress: &IngressHandle,
    snapshot: &Shared,
    backoff: &mut Backoff,
) -> Result<()> {
    connection_with_clock(
        saved,
        instance,
        ingress,
        snapshot,
        backoff,
        Clock::default(),
    )
    .await
}
async fn connection_with_clock(
    saved: &Saved,
    instance: Uuid,
    ingress: &IngressHandle,
    snapshot: &Shared,
    backoff: &mut Backoff,
    clock: Clock,
) -> Result<()> {
    let handshake = Lease::new(clock.clone(), CONNECT)
        .ok_or_else(|| anyhow::anyhow!("relay authorization clock unavailable"))?;
    let bound = saved
        .bound
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unbound"))?;
    let mut socket = connect(
        ws_url(&saved.server_url, "/relay/v1/tunnel")?,
        &bound.credential,
        protocol::MAX_FRAME,
    )
    .await?;
    if handshake.remaining().is_none() {
        bail!("relay handshake expired");
    }
    json(
        &mut socket,
        &Hello {
            version: protocol::VERSION,
            workbench_api_version: protocol::WORKBENCH_API_VERSION,
            gateway_id: bound.gateway_id,
            instance_id: instance,
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await?;
    let Some(Ok(Message::Text(text))) = timeout(CONNECT, socket.next()).await? else {
        bail!("missing relay welcome");
    };
    if handshake.remaining().is_none() {
        bail!("relay handshake expired");
    }
    let welcome: Welcome = serde_json::from_str(&text)?;
    if welcome.version != protocol::VERSION
        || welcome.account_id != bound.account_id
        || welcome.gateway_id != bound.gateway_id
        || welcome.gateway_origin != bound.gateway_origin
    {
        bail!("relay identity mismatch");
    }
    let lease_duration = Duration::from_secs(protocol::LEASE_SECONDS);
    let mut lease = Lease::new(clock, lease_duration)
        .ok_or_else(|| anyhow::anyhow!("relay authorization clock unavailable"))?;
    ingress.begin(
        welcome.account_id,
        welcome.gateway_id,
        welcome.connection_epoch,
        welcome.gateway_origin.clone(),
    )?;
    let _fence = Fence(ingress.clone());
    {
        let mut state = snapshot.lock().unwrap();
        state.state = "online".into();
        state.error = None;
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(protocol::RENEW_SECONDS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    backoff.connected(Instant::now());
    let mut streams = JoinSet::new();
    let mut active = HashMap::<Uuid, (Uuid, tokio::task::AbortHandle, bool)>::new();
    let mut seen = HashSet::new();
    loop {
        let remaining = lease
            .remaining()
            .ok_or_else(|| anyhow::anyhow!("relay control expired"))?;
        tokio::select! {
            biased;
            // Expiry wins over ready input, including after suspend/resume. It
            // must not wait for the next heartbeat tick or be renewed by stale Open.
            _=tokio::time::sleep(remaining)=> {
                bail!("relay control expired");
            }
            _=heartbeat.tick()=> {
                if lease.remaining().is_none() { bail!("relay control expired"); }
                json(&mut socket,&ToRelay::Heartbeat).await?;
            }
            result=streams.join_next(), if !streams.is_empty()=> {
                if let Some(Ok((id,failed)))=result { active.remove(&id); if failed { json(&mut socket,&ToRelay::StreamFailed{stream_id:id,code:"stream_failed".into()}).await?; } }
            }
            message=socket.next()=> {
                // On macOS the Tokio timer may not advance during sleep. Check
                // continuous age before accepting buffered Renew/Open/Ping.
                if lease.remaining().is_none() { bail!("relay control expired"); }
                let Some(message)=message else { bail!("relay closed"); };
                let message=message?;
                let Message::Text(text)=message else {
                    match message { Message::Ping(data)=> { send(&mut socket,Message::Pong(data)).await?; },Message::Pong(_)=>{},_=>bail!("invalid relay control") }
                    continue;
                };
                if text.len()>protocol::MAX_FRAME { bail!("relay control too large"); }
                let message:ToGateway=serde_json::from_str(&text)?;
                if !lease.renew(lease_duration) { bail!("relay control expired"); }
                backoff.control_received(Instant::now());
                match message {
                    ToGateway::Ping=>json(&mut socket,&ToRelay::Heartbeat).await?,
                    ToGateway::Renew{session_ids}=> { if session_ids.len()>4096 { bail!("too many renewed sessions"); } ingress.renew(&session_ids); },
                    ToGateway::Revoke{session_id}=> {
                        ingress.revoke(session_id);
                        active.retain(|_,(session,handle,_)| { if *session==session_id { handle.abort(); false } else { true } });
                    }
                    ToGateway::Open{request}=> {
                        let id=request.stream_id;
                        // The connection lifetime bounds replay bookkeeping as well as active streams.
                        if seen.len()>=100_000 { bail!("relay connection rotation required"); }
                        let terminal=matches!(request.kind,StreamKind::WebSocket);
                        let terminal_full=terminal && active.values().filter(|(_,_,terminal)|*terminal).count()>=protocol::MAX_WS_STREAMS;
                        if !seen.insert(id) || terminal_full || active.len()>=protocol::MAX_STREAMS || request.instance_id!=instance || validate_request(&request).is_err() {
                            json(&mut socket,&ToRelay::StreamFailed{stream_id:id,code:"request_rejected".into()}).await?; continue;
                        }
                        let capability=match ingress.grant(request.session_id) { Ok(capability)=>capability,Err(_)=> { json(&mut socket,&ToRelay::StreamFailed{stream_id:id,code:"session_revoked".into()}).await?; continue; } };
                        let cfg=saved.clone();let ingress=ingress.clone();let epoch=welcome.connection_epoch;let session=request.session_id;
                        let handle=streams.spawn(async move { let failed=stream(&cfg,epoch,&ingress,request,&capability).await.is_err(); (id,failed) });
                        active.insert(id,(session,handle,terminal));
                    }
                }
            }
        }
    }
}
fn validate_request(request: &Open) -> Result<()> {
    if !protocol::allowed(&request.method, &request.path)
        || request.headers.len() > 64
        || request
            .headers
            .iter()
            .map(|(n, v)| n.len() + v.len())
            .sum::<usize>()
            > 32 * 1024
    {
        bail!("invalid remote request");
    }
    for (name, value) in &request.headers {
        reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
        reqwest::header::HeaderValue::from_str(value)?;
    }
    if matches!(request.kind, StreamKind::WebSocket)
        && (request.method != "GET"
            || !request
                .path
                .split('?')
                .next()
                .unwrap_or("")
                .ends_with("/terminal"))
    {
        bail!("invalid remote websocket route");
    }
    Ok(())
}
fn request_headers(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
    let mut clean = reqwest::header::HeaderMap::new();
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "origin"
                | "if-match"
                | "user-agent"
                | "accept"
                | "content-type"
                | "last-event-id"
                | "if-none-match"
                | "if-modified-since"
                | "range"
                | "if-range"
                | "cache-control"
        ) && let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(lower.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            clean.append(name, value);
        }
    }
    clean
}
fn response_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            if matches!(
                name.as_str(),
                "content-type"
                    | "content-length"
                    | "content-range"
                    | "accept-ranges"
                    | "cache-control"
                    | "etag"
                    | "last-modified"
                    | "vary"
                    | "content-disposition"
                    | "x-content-type-options"
                    | "content-security-policy"
                    | "referrer-policy"
            ) {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().into(), value.into()))
            } else {
                None
            }
        })
        .collect()
}
async fn stream(
    saved: &Saved,
    epoch: Uuid,
    ingress: &IngressHandle,
    request: Open,
    capability: &str,
) -> Result<()> {
    let bound = saved
        .bound
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unbound"))?;
    let mut remote = connect(
        ws_url(
            &saved.server_url,
            &format!(
                "/relay/v1/tunnel/streams/{}?epoch={epoch}",
                request.stream_id
            ),
        )?,
        &bound.credential,
        if matches!(request.kind, StreamKind::WebSocket) {
            protocol::MAX_WS_MESSAGE
        } else {
            protocol::MAX_FRAME
        },
    )
    .await?;
    let origin = Url::parse(&bound.gateway_origin)?;
    let authority = match origin.port() {
        Some(port) => format!("{}:{port}", origin.host_str().unwrap_or("")),
        None => origin.host_str().unwrap_or("").to_owned(),
    };
    let address = ingress.address();
    if !address.ip().is_loopback() {
        bail!("private ingress must be loopback");
    }
    let mut headers = request_headers(&request.headers);
    headers.insert("host", authority.parse()?);
    headers.insert("authorization", format!("Bearer {capability}").parse()?);
    match request.kind {
        StreamKind::Http => {
            let body = timeout(CONNECT, receive_body(&mut remote)).await??;
            let client = local_client()?;
            let response = timeout(
                CONNECT,
                client
                    .request(
                        request.method.parse()?,
                        format!("http://{address}{}", request.path),
                    )
                    .headers(headers)
                    .body(body)
                    .send(),
            )
            .await??;
            json(
                &mut remote,
                &Data::ResponseHead {
                    status: response.status().as_u16(),
                    headers: response_headers(response.headers()),
                },
            )
            .await?;
            let mut body = response.bytes_stream();
            loop {
                tokio::select! {
                    message = remote.next() => {
                        // RequestEnd already ended the only permitted request.
                        // Continue handling websocket liveness and cancellation
                        // even when an SSE body has no events to produce.
                        match message {
                            Some(Ok(Message::Ping(data))) => send(&mut remote, Message::Pong(data)).await?,
                            Some(Ok(Message::Pong(_))) => {},
                            _ => bail!("response consumer disconnected or sent invalid data"),
                        }
                    }
                    chunk = body.next() => {
                        let Some(chunk) = chunk else { break; };
                        for part in chunk?.chunks(16 * 1024) {
                            send(&mut remote, Message::Binary(part.to_vec().into())).await?;
                        }
                    }
                }
            }
            json(&mut remote, &Data::ResponseEnd).await?;
            let _ = timeout(WRITE, remote.close(None)).await;
        }
        StreamKind::WebSocket => {
            let mut local_request =
                format!("ws://{address}{}", request.path).into_client_request()?;
            for (name, value) in &headers {
                local_request
                    .headers_mut()
                    .insert(name.clone(), value.clone());
            }
            let config = WebSocketConfig::default()
                .max_message_size(Some(protocol::MAX_WS_MESSAGE))
                .max_frame_size(Some(protocol::MAX_WS_MESSAGE))
                .write_buffer_size(0);
            let mut local = timeout(
                CONNECT,
                tokio_tungstenite::connect_async_with_config(local_request, Some(config), true),
            )
            .await??
            .0;
            json(&mut remote, &Data::WebSocketReady).await?;
            loop {
                tokio::select! {
                    message=remote.next()=> { match message { Some(Ok(Message::Text(text)))=>send(&mut local,Message::Text(text)).await?,Some(Ok(Message::Binary(data)))=>send(&mut local,Message::Binary(data)).await?,Some(Ok(Message::Ping(data)))=>send(&mut remote,Message::Pong(data)).await?,Some(Ok(Message::Pong(_)))=>{},_=>break } }
                    message=local.next()=> { match message { Some(Ok(Message::Text(text)))=>send(&mut remote,Message::Text(text)).await?,Some(Ok(Message::Binary(data)))=>send(&mut remote,Message::Binary(data)).await?,Some(Ok(Message::Ping(data)))=>send(&mut local,Message::Pong(data)).await?,Some(Ok(Message::Pong(_)))=>{},_=>break } }
                }
            }
            let _ = timeout(WRITE, local.close(None)).await;
            let _ = timeout(WRITE, remote.close(None)).await;
        }
    }
    Ok(())
}
async fn receive_body(socket: &mut Socket) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        match socket.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                if bytes.len() > protocol::MAX_FRAME
                    || body.len() + bytes.len() > protocol::MAX_HTTP_BODY
                {
                    bail!("remote body limit");
                }
                body.extend_from_slice(&bytes);
            }
            Some(Ok(Message::Text(text))) => {
                if matches!(serde_json::from_str::<Data>(&text)?, Data::RequestEnd) {
                    return Ok(body);
                }
                bail!("invalid request frame");
            }
            Some(Ok(Message::Ping(data))) => send(socket, Message::Pong(data)).await?,
            Some(Ok(Message::Pong(_))) => {}
            _ => bail!("request disconnected"),
        }
    }
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod reconnect_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_secure_origin_and_explicit_loopback() {
        assert!(server_url("https://relay.example", false).is_ok());
        for value in [
            "http://relay.example",
            "http://localhost",
            "https://u:p@relay.example",
            "https://relay.example/path",
            "https://relay.example?x=y",
            "ws://localhost/",
        ] {
            assert!(server_url(value, false).is_err(), "{value}");
        }
        for value in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://g.synthetic.localhost:8080",
        ] {
            assert!(server_url(value, true).is_ok(), "{value}");
        }
        assert!(server_url("http://127.0.0.1.evil.example", true).is_err());
    }
    #[test]
    fn strips_identity_headers_and_denies_unknown_routes() {
        let input = vec![
            ("Authorization".into(), "Bearer hostile".into()),
            ("Cookie".into(), "owner=hostile".into()),
            ("X-Forwarded-Host".into(), "localhost".into()),
            ("Host".into(), "localhost".into()),
            ("Origin".into(), "https://gateway.example".into()),
            ("Content-Type".into(), "application/json".into()),
        ];
        let clean = request_headers(&input);
        assert_eq!(clean.len(), 2);
        assert!(clean.contains_key("origin"));
        let mut request = Open {
            stream_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            kind: StreamKind::Http,
            method: "GET".into(),
            path: "/workbench/v1/application/connect".into(),
            headers: input,
        };
        assert!(validate_request(&request).is_err());
        request.path = "/workbench/v1/application".into();
        assert!(validate_request(&request).is_ok());
        request.kind = StreamKind::WebSocket;
        assert!(validate_request(&request).is_err());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::workbench::remote_access::Ingress;
    use axum::{
        Router,
        extract::{
            State, WebSocketUpgrade,
            ws::{Message as ServerMessage, WebSocket},
        },
        routing::get,
    };
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, oneshot};

    type ResultSender = Arc<Mutex<Option<oneshot::Sender<(u16, Vec<u8>)>>>>;

    #[derive(Clone)]
    struct Mock {
        welcome: Welcome,
        request: Open,
        actions: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ToGateway>>>,
        result: ResultSender,
    }
    async fn control(State(state): State<Mock>, ws: WebSocketUpgrade) -> axum::response::Response {
        ws.on_upgrade(move |mut socket|async move {
            let Some(Ok(ServerMessage::Text(hello)))=socket.recv().await else {return};
            let hello:Hello=serde_json::from_str(&hello).unwrap();assert_eq!(hello.instance_id,state.request.instance_id);
            socket.send(ServerMessage::Text(serde_json::to_string(&state.welcome).unwrap().into())).await.unwrap();
            socket.send(ServerMessage::Text(serde_json::to_string(&ToGateway::Open{request:state.request.clone()}).unwrap().into())).await.unwrap();
            loop { tokio::select! {
                command=async {state.actions.lock().await.recv().await} => { let Some(command)=command else {break}; if socket.send(ServerMessage::Text(serde_json::to_string(&command).unwrap().into())).await.is_err(){break;} },
                message=socket.recv()=> { if message.is_none() {break;} }
            } }
        })
    }
    async fn data(State(state): State<Mock>, ws: WebSocketUpgrade) -> axum::response::Response {
        ws.on_upgrade(move |socket| data_socket(state, socket))
    }
    async fn data_socket(state: Mock, mut socket: WebSocket) {
        if matches!(state.request.kind, StreamKind::WebSocket) {
            let Some(Ok(ServerMessage::Text(ready))) = socket.recv().await else {
                panic!("missing websocket ready")
            };
            assert!(matches!(
                serde_json::from_str::<Data>(&ready).unwrap(),
                Data::WebSocketReady
            ));
            let Some(Ok(ServerMessage::Text(snapshot))) = socket.recv().await else {
                panic!("missing terminal snapshot")
            };
            assert_eq!(snapshot.len(), 160 * 1024);
            assert!(snapshot.as_str().bytes().all(|byte| byte == b's'));
            let ack = r#"{"type":"ack","sequence":7}"#;
            socket.send(ServerMessage::Text(ack.into())).await.unwrap();
            assert_eq!(
                socket.recv().await.unwrap().unwrap(),
                ServerMessage::Text(ack.into())
            );
            let binary = vec![0, 1, 2, 255];
            socket
                .send(ServerMessage::Binary(binary.clone().into()))
                .await
                .unwrap();
            assert_eq!(
                socket.recv().await.unwrap().unwrap(),
                ServerMessage::Binary(binary.clone().into())
            );
            if let Some(tx) = state.result.lock().unwrap().take() {
                let _ = tx.send((101, binary));
            }
            return;
        }
        socket
            .send(ServerMessage::Text(
                serde_json::to_string(&Data::RequestEnd).unwrap().into(),
            ))
            .await
            .unwrap();
        let mut status = 0;
        let mut body = Vec::new();
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                ServerMessage::Text(text) => match serde_json::from_str::<Data>(&text).unwrap() {
                    Data::ResponseHead {
                        status: code,
                        headers,
                    } => {
                        status = code;
                        assert!(!headers.iter().any(|(key, _)| key == "set-cookie"));
                    }
                    Data::ResponseEnd => break,
                    _ => panic!("unexpected frame"),
                },
                ServerMessage::Binary(bytes) => body.extend_from_slice(&bytes),
                _ => {}
            }
        }
        if let Some(tx) = state.result.lock().unwrap().take() {
            let _ = tx.send((status, body));
        }
    }
    #[tokio::test]
    async fn outbound_stream_uses_private_ingress_and_revoke_fences_session() {
        let instance = Uuid::new_v4();
        let session = Uuid::new_v4();
        let ingress = Ingress::start(
            Router::new().route(
                "/workbench/v1/application",
                get(|| async {
                    (
                        [(axum::http::header::SET_COOKIE, "private=never-forward")],
                        "synthetic-local-application",
                    )
                }),
            ),
            instance,
        )
        .unwrap();
        let (actions, rx) = mpsc::unbounded_channel();
        let (result, result_rx) = oneshot::channel();
        let welcome = Welcome {
            version: protocol::VERSION,
            account_id: Uuid::new_v4(),
            gateway_id: Uuid::new_v4(),
            connection_epoch: Uuid::new_v4(),
            gateway_origin: "https://gateway.example".into(),
        };
        let state = Mock {
            welcome: welcome.clone(),
            request: Open {
                stream_id: Uuid::new_v4(),
                session_id: session,
                instance_id: instance,
                kind: StreamKind::Http,
                method: "GET".into(),
                path: "/workbench/v1/application".into(),
                headers: vec![
                    ("Cookie".into(), "hostile-owner".into()),
                    ("Authorization".into(), "Bearer hostile".into()),
                ],
            },
            actions: Arc::new(tokio::sync::Mutex::new(rx)),
            result: Arc::new(Mutex::new(Some(result))),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/relay/v1/tunnel", get(control))
                    .route("/relay/v1/tunnel/streams/{id}", get(data))
                    .with_state(state),
            )
            .await
            .unwrap()
        });
        let saved = Saved {
            server_url: format!("http://relay.synthetic.localhost:{}/", address.port()),
            name: "Synthetic".into(),
            allow_insecure_loopback: true,
            enabled: true,
            authentication_required: false,
            bound: Some(protocol::Bound {
                username: "synthetic".into(),
                account_id: welcome.account_id,
                gateway_id: welcome.gateway_id,
                credential: "synthetic-test-credential".into(),
                gateway_origin: welcome.gateway_origin,
            }),
        };
        let view = Arc::new(Mutex::new(super::super::Snapshot::from_saved(&saved)));
        let handle = ingress.handle();
        let connector_handle = handle.clone();
        let connector =
            tokio::spawn(
                async move { connection(&saved, instance, &connector_handle, &view).await },
            );
        let (status, body) = timeout(Duration::from_secs(5), result_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"synthetic-local-application");
        assert!(handle.grant(session).is_ok());
        actions
            .send(ToGateway::Revoke {
                session_id: session,
            })
            .unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                if handle.grant(session).is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        connector.abort();
        let _ = connector.await;
        assert!(handle.grant(Uuid::new_v4()).is_err());
        server.abort();
    }
    #[tokio::test]
    async fn terminal_preserves_text_ack_and_binary_without_replay() {
        let instance = Uuid::new_v4();
        let run = Uuid::new_v4();
        let path = format!("/workbench/v1/runs/{run}/terminal");
        let local = Router::new().route(
            &path,
            get(|ws: WebSocketUpgrade| async {
                ws.on_upgrade(|mut socket| async move {
                    socket
                        .send(ServerMessage::Text("s".repeat(160 * 1024).into()))
                        .await
                        .unwrap();
                    while let Some(Ok(message)) = socket.recv().await {
                        if matches!(message, ServerMessage::Text(_) | ServerMessage::Binary(_))
                            && socket.send(message).await.is_err()
                        {
                            break;
                        }
                    }
                })
            }),
        );
        let ingress = Ingress::start(local, instance).unwrap();
        let (_actions, rx) = mpsc::unbounded_channel();
        let (result, result_rx) = oneshot::channel();
        let welcome = Welcome {
            version: protocol::VERSION,
            account_id: Uuid::new_v4(),
            gateway_id: Uuid::new_v4(),
            connection_epoch: Uuid::new_v4(),
            gateway_origin: "https://gateway.example".into(),
        };
        let state = Mock {
            welcome: welcome.clone(),
            request: Open {
                stream_id: Uuid::new_v4(),
                session_id: Uuid::new_v4(),
                instance_id: instance,
                kind: StreamKind::WebSocket,
                method: "GET".into(),
                path,
                headers: vec![("Origin".into(), "https://gateway.example".into())],
            },
            actions: Arc::new(tokio::sync::Mutex::new(rx)),
            result: Arc::new(Mutex::new(Some(result))),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/relay/v1/tunnel", get(control))
                    .route("/relay/v1/tunnel/streams/{id}", get(data))
                    .with_state(state),
            )
            .await
            .unwrap()
        });
        let saved = Saved {
            server_url: format!("http://{address}/"),
            name: "Synthetic".into(),
            allow_insecure_loopback: true,
            enabled: true,
            authentication_required: false,
            bound: Some(protocol::Bound {
                username: "synthetic".into(),
                account_id: welcome.account_id,
                gateway_id: welcome.gateway_id,
                credential: "synthetic-test-credential".into(),
                gateway_origin: welcome.gateway_origin,
            }),
        };
        let view = Arc::new(Mutex::new(super::super::Snapshot::from_saved(&saved)));
        let handle = ingress.handle();
        let connector =
            tokio::spawn(async move { connection(&saved, instance, &handle, &view).await });
        let (status, body) = timeout(Duration::from_secs(5), result_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, 101);
        assert_eq!(body, vec![0, 1, 2, 255]);
        connector.abort();
        let _ = connector.await;
        server.abort();
    }
}
