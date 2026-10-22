use crate::{App, Failure, Result, bad, denied};
use axum::{
    Json,
    extract::{
        Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::Response,
};
use codex_relay_protocol::{
    self as protocol, Hello, MAX_FRAME, MAX_STREAMS, ToGateway, ToRelay, Welcome,
};
use futures_util::{SinkExt, StreamExt};
use protocol::secure::{AccessContext, ChannelBinding, Scope};
use serde::Deserialize;
use std::{collections::HashSet, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct AdmissionGate {
    pub credential: String,
    pub reached: std::sync::Arc<tokio::sync::Notify>,
    pub resume: std::sync::Arc<tokio::sync::Notify>,
}

pub(crate) struct Connection {
    pub credential: String,
    pub workbench_api_version: u32,
    pub epoch: Uuid,
    pub instance: Uuid,
    pub sessions: HashSet<Uuid>,
    pub capacity: std::sync::Arc<tokio::sync::Semaphore>,
    pub send: mpsc::Sender<ToGateway>,
    pub cancel: watch::Sender<bool>,
}
pub(crate) struct Stream {
    pub gateway: Uuid,
    pub epoch: Uuid,
    pub session: Uuid,
    pub cancel: watch::Sender<bool>,
    attach: Option<oneshot::Sender<WebSocket>>,
}
fn bearer(h: &HeaderMap) -> Result<&str> {
    if h.get_all("authorization").iter().count() != 1 {
        return Err(denied());
    }
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() <= 128)
        .ok_or_else(denied)
}
pub(crate) async fn connect(
    State(s): State<App>,
    h: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    let token = bearer(&h)?.to_string();
    let (gateway, account) = s.0.store.gateway_auth(&token).ok_or_else(denied)?;
    let global_permit =
        s.0.control_capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    let gateway_permit = {
        let mut capacities = s.0.gateway_control_capacity.lock().unwrap();
        capacities.retain(|_, v| v.strong_count() > 0);
        let semaphore = capacities
            .get(&gateway)
            .and_then(std::sync::Weak::upgrade)
            .unwrap_or_else(|| {
                let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
                capacities.insert(gateway, std::sync::Arc::downgrade(&sem));
                sem
            });
        semaphore
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?
    };
    Ok(ws
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| async move {
            let _permits = (global_permit, gateway_permit);
            control(s, socket, token, gateway, account).await;
        }))
}
async fn control(s: App, mut socket: WebSocket, credential: String, gateway: Uuid, account: Uuid) {
    let hello = match tokio::time::timeout(Duration::from_secs(10), socket.recv()).await {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Hello>(&t).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|h| {
        h.version == protocol::VERSION
            && h.gateway_id == gateway
            && !h.instance_id.is_nil()
            && h.client_version.len() <= 128
    }) else {
        return;
    };
    if s.0.store.gateway_auth(&credential) != Some((gateway, account)) {
        return;
    }
    #[cfg(test)]
    {
        let gate = s.0.admission_gate.lock().unwrap().clone();
        if let Some(gate) = gate.filter(|gate| gate.credential == credential) {
            gate.reached.notify_one();
            gate.resume.notified().await;
        }
    }
    let epoch = Uuid::new_v4();
    let (send, mut receive) = mpsc::channel(128);
    let (cancel, mut stop) = watch::channel(false);
    {
        // Serialize identity validation and installation with gateway deletion.
        // Lock order is tunnels -> store; never let a revoked credential replace a rebind.
        let mut tunnels = s.0.tunnels.lock().unwrap();
        if s.0.store.gateway_auth(&credential) != Some((gateway, account)) {
            return;
        }
        if s.0
            .store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE gateways SET workbench_api_version=?1 WHERE id=?2 AND credential=?3",
                rusqlite::params![
                    // Zero in storage means no handshake yet. Keep an explicitly
                    // unversioned legacy Hello distinguishable after disconnect.
                    if hello.workbench_api_version == 0 {
                        u32::MAX
                    } else {
                        hello.workbench_api_version
                    },
                    gateway.to_string(),
                    crate::store::digest(&credential)
                ],
            )
            .ok()
            != Some(1)
        {
            return;
        }
        cancel_streams(&s, gateway, None);
        // Physical channels from the previous epoch still hold their permits
        // until their sockets drop. Reconnect must not reset this budget.
        let capacity = {
            let mut capacities = s.0.gateway_channel_capacity.lock().unwrap();
            capacities.retain(|_, v| v.strong_count() > 0);
            capacities
                .get(&gateway)
                .and_then(std::sync::Weak::upgrade)
                .unwrap_or_else(|| {
                    let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS));
                    capacities.insert(gateway, std::sync::Arc::downgrade(&capacity));
                    capacity
                })
        };
        if let Some(old) = tunnels.insert(
            gateway,
            Connection {
                credential: credential.clone(),
                workbench_api_version: hello.workbench_api_version,
                epoch,
                instance: hello.instance_id,
                sessions: HashSet::new(),
                capacity,
                send,
                cancel,
            },
        ) {
            old.cancel.send_replace(true);
        }
    }
    let Some(gateway_origin) = s.0.config.gateway_origin(gateway) else {
        cleanup(&s, gateway, epoch);
        return;
    };
    let welcome = Welcome {
        version: protocol::VERSION,
        account_id: account,
        gateway_id: gateway,
        connection_epoch: epoch,
        gateway_origin,
    };
    if send_json(&mut socket, &welcome).await.is_err() {
        cleanup(&s, gateway, epoch);
        return;
    }
    let mut renew = tokio::time::interval(Duration::from_secs(protocol::RENEW_SECONDS));
    let mut sweep = tokio::time::interval(Duration::from_secs(1));
    let mut heartbeat = tokio::time::Instant::now();
    loop {
        let outgoing = tokio::select! {
            _=stop.changed()=>break,
            _=sweep.tick()=>{
                if s.0.store.cleanup_expired_if_due().is_err(){eprintln!("relay metadata cleanup failed");}
                s.revoke_invalid();
                if s.0.store.gateway_auth(&credential)!=Some((gateway,account)) || heartbeat.elapsed()>Duration::from_secs(protocol::LEASE_SECONDS){break;}
                continue;
            },
            _=renew.tick()=>{
                let ids=s.0.tunnels.lock().unwrap().get(&gateway).filter(|c|c.epoch==epoch).map(|c|c.sessions.iter().copied().collect::<Vec<_>>()).unwrap_or_default();
                Some(ToGateway::Renew{session_ids:ids})
            },
            msg=receive.recv()=>msg,
            msg=socket.recv()=>{
                match msg {
                    Some(Ok(Message::Text(text)))=>match serde_json::from_str::<ToRelay>(&text){
                        Ok(ToRelay::Heartbeat)=>heartbeat=tokio::time::Instant::now(),
                        Ok(ToRelay::ChannelFailed{channel_id:stream_id,..})=>{let mut streams=s.0.streams.lock().unwrap();if streams.get(&stream_id).is_some_and(|v|v.gateway==gateway&&v.epoch==epoch) && let Some(v)=streams.remove(&stream_id){v.cancel.send_replace(true);}},
                        _=>break,
                    },
                    Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},
                    _=>break,
                } continue;
            }
        };
        let Some(outgoing) = outgoing else { break };
        // Do not send queued grants or renewals from a replaced connection.
        if *stop.borrow()
            || s.0
                .tunnels
                .lock()
                .unwrap()
                .get(&gateway)
                .is_none_or(|c| c.epoch != epoch)
        {
            break;
        }
        if tokio::select! {_=stop.changed()=>true,result=send_json(&mut socket,&outgoing)=>result.is_err()}
        {
            break;
        }
    }
    cleanup(&s, gateway, epoch);
}
fn cleanup(s: &App, gateway: Uuid, epoch: Uuid) {
    {
        let mut tunnels = s.0.tunnels.lock().unwrap();
        if tunnels.get(&gateway).is_some_and(|c| c.epoch == epoch) {
            tunnels.remove(&gateway);
        }
    }
    cancel_streams(s, gateway, Some(epoch));
}
fn cancel_streams(s: &App, gateway: Uuid, epoch: Option<Uuid>) {
    s.0.streams.lock().unwrap().retain(|_, v| {
        if v.gateway == gateway && epoch.is_none_or(|e| e == v.epoch) {
            v.cancel.send_replace(true);
            false
        } else {
            true
        }
    });
}
async fn send_json<T: serde::Serialize>(ws: &mut WebSocket, v: &T) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(v).map_err(|_| ())?;
    tokio::time::timeout(Duration::from_secs(10), ws.send(Message::Text(text.into())))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Epoch {
    epoch: Uuid,
}
pub(crate) async fn attach(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Query(query): Query<Epoch>,
    h: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    let (gateway, _) = s.0.store.gateway_auth(bearer(&h)?).ok_or_else(denied)?;
    let tunnels = s.0.tunnels.lock().unwrap();
    let c = tunnels
        .get(&gateway)
        .filter(|c| c.epoch == query.epoch)
        .ok_or_else(denied)?;
    if *c.cancel.borrow() {
        return Err(denied());
    }
    let mut streams = s.0.streams.lock().unwrap();
    let p = streams
        .get_mut(&id)
        .filter(|v| v.gateway == gateway && v.epoch == query.epoch)
        .ok_or_else(denied)?;
    if !s.0.store.active(p.session) || *p.cancel.borrow() {
        return Err(denied());
    }
    let send = p.attach.take().ok_or_else(denied)?;
    Ok(ws
        .max_frame_size(MAX_RECORD)
        .max_message_size(MAX_RECORD)
        .on_upgrade(move |socket| async move {
            let _ = send.send(socket);
        }))
}

/// This endpoint can only describe an existing page grant; it never creates one.
pub(crate) async fn access(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
    req: Request,
) -> Result<Json<AccessContext>> {
    if req.uri().path() != format!("/relay/v1/gateways/{id}/access") {
        return Err(bad());
    }
    let (grant, remaining) = crate::routing::selectors(&h, req.uri().query(), false)?;
    if !remaining.is_empty() {
        return Err(bad());
    }
    crate::routing::grant_session(&s, &h, id, grant)?;
    let tunnels = s.0.tunnels.lock().unwrap();
    let c = tunnels
        .get(&id)
        .ok_or(Failure(StatusCode::SERVICE_UNAVAILABLE))?;
    Ok(Json(context(&s, &h, id, grant, c)?))
}

fn context(
    s: &App,
    h: &HeaderMap,
    gateway: Uuid,
    grant: Uuid,
    c: &Connection,
) -> Result<AccessContext> {
    let parent = s.authorized_portal_session(h)?;
    let session = crate::routing::grant_session(s, h, gateway, grant)?;
    if c.workbench_api_version != protocol::WORKBENCH_API_VERSION {
        return Err(Failure(StatusCode::CONFLICT));
    }
    if *c.cancel.borrow()
        || s.0.store.gateway_auth(&c.credential) != Some((gateway, session.account))
    {
        return Err(denied());
    }
    Ok(AccessContext {
        wire_version: protocol::VERSION,
        api_version: protocol::WORKBENCH_API_VERSION,
        account_id: session.account,
        parent_session_id: parent.id,
        grant_id: grant,
        gateway_id: gateway,
        instance_id: c.instance,
        connection_epoch: c.epoch,
    })
}

fn channel_scope(query: &str) -> Result<Scope> {
    let mut scope = None;
    let mut run = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').ok_or_else(bad)?;
        match key {
            "scope" if scope.is_none() => scope = Some(value),
            "runId" if run.is_none() => run = Some(value),
            _ => return Err(bad()),
        }
    }
    match (scope, run) {
        (Some("application"), None) => Ok(Scope::Application),
        (Some("run"), Some(raw)) => {
            let run_id = Uuid::parse_str(raw).map_err(|_| bad())?;
            if run_id.is_nil() || raw != run_id.to_string() {
                return Err(bad());
            }
            Ok(Scope::Run { run_id })
        }
        _ => Err(bad()),
    }
}

pub(crate) async fn secure_channel(
    State(s): State<App>,
    h: HeaderMap,
    ws: WebSocketUpgrade,
    req: Request,
) -> Result<Response> {
    let (gateway, tail) = crate::routing::canonical_gateway(req.uri().path())?;
    if tail != "workbench/v1/secure-channel" {
        return Err(bad());
    }
    let (grant, query) = crate::routing::selectors(&h, req.uri().query(), true)?;
    let scope = channel_scope(&query)?;
    // Revocation must remain visible even when the gateway is offline or capacity
    // is exhausted. Admission below rechecks under the control installation lock.
    crate::routing::grant_session(&s, &h, gateway, grant)?;
    let global_permit =
        s.0.stream_capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    let id = Uuid::new_v4();
    let (attachment, receiver) = oneshot::channel();
    let (cancel, mut stop) = watch::channel(false);
    let (binding, gateway_permit) = {
        // Recheck the original parent, grant, credential and epoch at admission.
        // This follows the control installation lock order: tunnels -> store.
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let c = tunnels
            .get_mut(&gateway)
            .ok_or(Failure(StatusCode::SERVICE_UNAVAILABLE))?;
        let context = context(&s, &h, gateway, grant, c)?;
        let permit = c
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
        let mut streams = s.0.streams.lock().unwrap();
        if streams.values().filter(|v| v.gateway == gateway).count() >= MAX_STREAMS
            || (c.sessions.len() >= 256 && !c.sessions.contains(&grant))
        {
            return Err(Failure(StatusCode::TOO_MANY_REQUESTS));
        }
        let binding = ChannelBinding {
            context,
            channel_id: id,
            scope,
        };
        c.sessions.insert(grant);
        streams.insert(
            id,
            Stream {
                gateway,
                epoch: c.epoch,
                session: grant,
                cancel,
                attach: Some(attachment),
            },
        );
        if c.send
            .try_send(ToGateway::OpenChannel {
                binding: binding.clone(),
            })
            .is_err()
        {
            streams.remove(&id);
            return Err(Failure(StatusCode::SERVICE_UNAVAILABLE));
        }
        (binding, permit)
    };
    let guard = StreamGuard {
        s,
        id,
        _gateway_permit: gateway_permit,
        _global_permit: global_permit,
    };
    let agent = tokio::select! {
        biased;
        _=stop.changed()=>return Err(denied()),
        result=tokio::time::timeout(Duration::from_secs(10),receiver)=>
            result.map_err(|_|Failure(StatusCode::GATEWAY_TIMEOUT))?
                .map_err(|_|Failure(StatusCode::BAD_GATEWAY))?
    };
    Ok(ws
        .max_frame_size(MAX_RECORD)
        .max_message_size(MAX_RECORD)
        .on_upgrade(move |public| bridge(public, agent, stop, guard, binding)))
}

struct StreamGuard {
    s: App,
    id: Uuid,
    _gateway_permit: tokio::sync::OwnedSemaphorePermit,
    _global_permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.s.0.streams.lock().unwrap().remove(&self.id);
    }
}

// Opaque records are bounded independently in each direction. A blocked writer
// never stops the opposite reader/writer or the gateway control connection.
const MAX_RECORD: usize = protocol::secure::WIRE_RECORD;
const MAX_HANDSHAKE: usize = protocol::secure::HANDSHAKE;
const QUEUE_BYTES: usize = 128 * 1024;
const QUEUE_RECORDS: usize = 32;
struct Queued {
    message: Message,
    _bytes: tokio::sync::OwnedSemaphorePermit,
}

fn enqueue(
    tx: &mpsc::Sender<Queued>,
    bytes: &std::sync::Arc<tokio::sync::Semaphore>,
    message: Message,
    size: usize,
) -> std::result::Result<(), ()> {
    let permit = bytes
        .clone()
        .try_acquire_many_owned(size.max(1) as u32)
        .map_err(|_| ())?;
    tx.try_send(Queued {
        message,
        _bytes: permit,
    })
    .map_err(|_| ())
}

async fn pump(
    mut source: futures_util::stream::SplitStream<WebSocket>,
    mut target: futures_util::stream::SplitSink<WebSocket, Message>,
) {
    let bytes = std::sync::Arc::new(tokio::sync::Semaphore::new(QUEUE_BYTES));
    let (tx, mut rx) = mpsc::channel::<Queued>(QUEUE_RECORDS);
    let read = async move {
        let mut text_count = 0;
        let mut binary_seen = false;
        let deadline = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(deadline);
        loop {
            let message = tokio::select! {
                _ = &mut deadline, if !binary_seen => break,
                message = source.next() => message,
            };
            let Some(Ok(message)) = message else { break };
            let size = match &message {
                Message::Text(t) if !binary_seen && t.len() <= MAX_HANDSHAKE && text_count < 2 => {
                    text_count += 1;
                    t.len()
                }
                Message::Binary(b) if b.len() <= MAX_RECORD => {
                    binary_seen = true;
                    b.len()
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                _ => break,
            };
            // Fail this channel at the boundary; never accumulate unbounded data.
            if enqueue(&tx, &bytes, message, size).is_err() {
                break;
            }
        }
    };
    let write = async move {
        while let Some(record) = rx.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(10), target.send(record.message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    };
    tokio::select! { _=read=>{}, _=write=>{} }
}

async fn bridge(
    mut public: WebSocket,
    agent: WebSocket,
    mut stop: watch::Receiver<bool>,
    _guard: StreamGuard,
    binding: ChannelBinding,
) {
    if *stop.borrow() {
        return;
    }
    let envelope = serde_json::json!({"type":"channel_binding","binding":binding});
    if tokio::select! {
        biased;
        _=stop.changed()=>true,
        result=send_json(&mut public,&envelope)=>result.is_err(),
    } {
        return;
    }
    let (public_write, public_read) = public.split();
    let (agent_write, agent_read) = agent.split();
    tokio::select! {
        biased;
        _=stop.changed()=>{},
        _=pump(public_read, agent_write)=>{},
        _=pump(agent_read, public_write)=>{},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn queue_bounds_bytes_including_inflight_and_zero_length_record_count() {
        let bytes = std::sync::Arc::new(tokio::sync::Semaphore::new(QUEUE_BYTES));
        let (tx, mut rx) = mpsc::channel(QUEUE_RECORDS);
        for _ in 0..2 {
            assert!(
                enqueue(
                    &tx,
                    &bytes,
                    Message::Binary(vec![0; 64 * 1024].into()),
                    64 * 1024
                )
                .is_ok()
            );
        }
        assert_eq!(bytes.available_permits(), 0);
        let inflight = rx.recv().await.unwrap();
        assert!(enqueue(&tx, &bytes, Message::Binary(vec![0].into()), 1).is_err());
        drop(inflight);
        drop(rx.recv().await.unwrap());
        assert_eq!(bytes.available_permits(), QUEUE_BYTES);
        for _ in 0..QUEUE_RECORDS {
            assert!(enqueue(&tx, &bytes, Message::Binary(Vec::new().into()), 0).is_ok());
        }
        assert!(enqueue(&tx, &bytes, Message::Binary(Vec::new().into()), 0).is_err());
        drop(rx);
        assert_eq!(bytes.available_permits(), QUEUE_BYTES);
    }
    #[test]
    fn scope_is_exact_and_contains_no_business_selectors() {
        assert!(matches!(
            channel_scope("scope=application"),
            Ok(Scope::Application)
        ));
        let run = Uuid::new_v4();
        assert!(
            matches!(channel_scope(&format!("scope=run&runId={run}")), Ok(Scope::Run {run_id}) if run_id == run)
        );
        for query in [
            "scope=run",
            "scope=application&runId=x",
            "scope=application&scope=application",
            "scope=application&path=secret",
            "scope=run&runId=00000000-0000-0000-0000-000000000000",
            "scope=%61pplication",
            "scope=application&",
            "scope=other",
        ] {
            assert!(channel_scope(query).is_err(), "{query}");
        }
    }
}
