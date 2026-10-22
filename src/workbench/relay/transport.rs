use super::{Shared, storage::Saved};
use crate::workbench::lease_clock::{Clock, Lease};
use crate::workbench::remote_access::IngressHandle;
use anyhow::{Result, bail};
use codex_relay_protocol::{self as protocol, Hello, ToGateway, ToRelay, Welcome};
#[cfg(test)]
use codex_relay_protocol::{Open, StreamKind};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Url};
use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::Arc,
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
pub(super) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const CONNECT: Duration = Duration::from_secs(10);
const WRITE: Duration = Duration::from_secs(2);

struct Backoff {
    ceiling_ms: u64,
    connected_at: Option<Instant>,
    limits: Arc<super::channel::Limits>,
}
impl Default for Backoff {
    fn default() -> Self {
        Self {
            ceiling_ms: 1_000,
            connected_at: None,
            limits: Arc::new(super::channel::Limits::default()),
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
pub(super) fn local_client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT)
        .build()?)
}
pub(super) fn ws_url(base: &str, path: &str) -> Result<String> {
    let mut url = Url::parse(base)?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid socket origin"))?;
    let url = url.join(path)?;
    Ok(url.into())
}
pub(super) async fn connect(url: String, credential: &str, max_message: usize) -> Result<Socket> {
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
pub(super) async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    timeout(WRITE, socket.send(message)).await??;
    Ok(())
}
pub(super) async fn json(socket: &mut Socket, value: &impl serde::Serialize) -> Result<()> {
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
#[cfg(test)]
pub(super) async fn run(
    saved: Saved,
    instance: Uuid,
    ingress: IngressHandle,
    snapshot: Shared,
) -> bool {
    run_with_limits(
        saved,
        instance,
        ingress,
        snapshot,
        Arc::new(super::channel::Limits::default()),
    )
    .await
}
pub(super) async fn run_with_limits(
    saved: Saved,
    instance: Uuid,
    ingress: IngressHandle,
    snapshot: Shared,
    limits: Arc<super::channel::Limits>,
) -> bool {
    let mut backoff = Backoff {
        limits,
        ..Backoff::default()
    };
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
    // Old aborted channel tasks may not have dropped their permits yet. Keep
    // endpoint quotas continuous across control epochs and reconnect attempts.
    let limits = backoff.limits.clone();
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
                if let Some(Ok((id,failed)))=result { active.remove(&id); if failed { json(&mut socket,&ToRelay::ChannelFailed{channel_id:id,code:"channel_failed".into()}).await?; } }
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
                    ToGateway::Open{..}=>bail!("plaintext_protocol_rejected"),
                    ToGateway::OpenChannel{binding}=> {
                        let id=binding.channel_id;let c=&binding.context;
                        if seen.len()>=100_000 { bail!("relay connection rotation required"); }
                        if !binding.valid()||!seen.insert(id)||active.len()>=64||c.instance_id!=instance||c.account_id!=welcome.account_id||c.gateway_id!=welcome.gateway_id||c.connection_epoch!=welcome.connection_epoch {
                            json(&mut socket,&ToRelay::ChannelFailed{channel_id:id,code:"channel_rejected".into()}).await?;continue;
                        }
                        let session=c.grant_id;
                        let capability=match ingress.grant(session) {Ok(capability)=>capability,Err(_)=>{json(&mut socket,&ToRelay::ChannelFailed{channel_id:id,code:"session_revoked".into()}).await?;continue;}};
                        let cfg=saved.clone();let ingress=ingress.clone();let limits=limits.clone();
                        let handle=streams.spawn(async move {let failed=super::channel::run(&cfg,ingress,binding,capability,limits).await.is_err();(id,failed)});
                        active.insert(id,(session,handle,false));
                    }
                }
            }
        }
    }
}
#[cfg(test)]
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
pub(super) fn request_headers(headers: &[(String, String)]) -> reqwest::header::HeaderMap {
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
