use crate::{App, Failure, Result, bad, cookie, denied};
use axum::{
    body::{Body, Bytes},
    extract::{
        Path, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use codex_relay_protocol::{
    self as protocol, Data, Hello, MAX_FRAME, MAX_STREAMS, Open, StreamKind, ToGateway, ToRelay,
    Welcome,
};
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
    pub terminal_capacity: std::sync::Arc<tokio::sync::Semaphore>,
    pub send: mpsc::Sender<ToGateway>,
    pub cancel: watch::Sender<bool>,
}
pub(crate) struct Stream {
    pub gateway: Uuid,
    pub epoch: Uuid,
    pub session: Uuid,
    pub terminal: bool,
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
        h.version == protocol::VERSION && h.gateway_id == gateway && h.client_version.len() <= 128
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
        if let Some(old) = tunnels.insert(
            gateway,
            Connection {
                credential: credential.clone(),
                workbench_api_version: hello.workbench_api_version,
                epoch,
                instance: hello.instance_id,
                sessions: HashSet::new(),
                capacity: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS)),
                terminal_capacity: std::sync::Arc::new(tokio::sync::Semaphore::new(4)),
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
                if s.0.store.gateway_auth(&credential)!=Some((gateway,account)) || heartbeat.elapsed()>Duration::from_secs(40){break;}
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
                        Ok(ToRelay::StreamFailed{stream_id,..})=>{let mut streams=s.0.streams.lock().unwrap();if streams.get(&stream_id).is_some_and(|v|v.gateway==gateway&&v.epoch==epoch) && let Some(v)=streams.remove(&stream_id){v.cancel.send_replace(true);}},
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
    if tunnels.get(&gateway).is_none_or(|c| c.epoch != query.epoch) {
        return Err(denied());
    }
    let mut streams = s.0.streams.lock().unwrap();
    let p = streams
        .get_mut(&id)
        .filter(|v| v.gateway == gateway && v.epoch == query.epoch)
        .ok_or_else(denied)?;
    if !s.0.store.active(p.session) {
        return Err(denied());
    }
    let max_message = if p.terminal {
        protocol::MAX_WS_MESSAGE
    } else {
        MAX_FRAME
    };
    let send = p.attach.take().ok_or_else(denied)?;
    Ok(ws
        .max_frame_size(max_message)
        .max_message_size(max_message)
        .on_upgrade(move |socket| async move {
            let _ = send.send(socket);
        }))
}
struct StreamGuard {
    s: App,
    id: Uuid,
    _gateway_permit: tokio::sync::OwnedSemaphorePermit,
    _global_permit: tokio::sync::OwnedSemaphorePermit,
    _gateway_terminal_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    _global_terminal_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.s.0.streams.lock().unwrap().remove(&self.id);
    }
}
pub(crate) async fn forward(
    State(s): State<App>,
    h: HeaderMap,
    ws: std::result::Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    req: Request,
) -> Response {
    forward_inner(s, h, req, ws.ok())
        .await
        .unwrap_or_else(IntoResponse::into_response)
}
pub(crate) async fn forward_inner(
    s: App,
    h: HeaderMap,
    req: Request,
    ws: Option<WebSocketUpgrade>,
) -> Result<Response> {
    let method = req.method().as_str().to_string();
    let (gateway, session, path) = if s.0.config.single_origin {
        if !req.uri().path().starts_with("/g/") {
            return Err(Failure(StatusCode::NOT_FOUND));
        }
        let (gateway, tail) = crate::routing::canonical_gateway(req.uri().path())?;
        if !tail.starts_with("workbench/v1/") {
            return Err(Failure(StatusCode::NOT_FOUND));
        }
        let streaming = tail.ends_with("/live/events") || tail.ends_with("/terminal");
        let (grant, query) = crate::routing::selectors(&h, req.uri().query(), streaming)?;
        let session = crate::routing::grant_session(&s, &h, gateway, grant)?;
        let path = format!(
            "/{tail}{}",
            if query.is_empty() {
                String::new()
            } else {
                format!("?{query}")
            }
        );
        (gateway, session, path)
    } else {
        let gateway =
            s.0.config
                .host_origin(&h)
                .and_then(|(_, g)| g)
                .ok_or_else(denied)?;
        let session =
            s.0.store
                .session(
                    &cookie(&h, "__Host-workbench_session").ok_or_else(denied)?,
                    Some(gateway),
                )
                .ok_or_else(denied)?;
        let path = req
            .uri()
            .path_and_query()
            .ok_or_else(bad)?
            .as_str()
            .to_string();
        (gateway, session, path)
    };
    if !protocol::allowed(&method, &path) {
        return Err(Failure(StatusCode::NOT_FOUND));
    }
    let terminal = ws.is_some();
    if terminal && !(req.uri().path().ends_with("/terminal") && method == "GET") {
        return Err(bad());
    }
    let (gateway_permit, gateway_terminal_permit, admitted_epoch) = {
        let tunnels = s.0.tunnels.lock().unwrap();
        let c = tunnels
            .get(&gateway)
            .ok_or(Failure(StatusCode::SERVICE_UNAVAILABLE))?;
        if c.workbench_api_version != protocol::WORKBENCH_API_VERSION {
            return Err(Failure(StatusCode::CONFLICT));
        }
        if s.0.store.gateway_auth(&c.credential).is_none() {
            return Err(denied());
        }
        (
            c.capacity
                .clone()
                .try_acquire_owned()
                .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?,
            if terminal {
                Some(
                    c.terminal_capacity
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?,
                )
            } else {
                None
            },
            c.epoch,
        )
    };
    let global_permit =
        s.0.stream_capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    let global_terminal_permit = if terminal {
        Some(
            s.0.terminal_capacity
                .clone()
                .try_acquire_owned()
                .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?,
        )
    } else {
        None
    };
    let id = Uuid::new_v4();
    let guard = StreamGuard {
        s: s.clone(),
        id,
        _gateway_permit: gateway_permit,
        _global_permit: global_permit,
        _gateway_terminal_permit: gateway_terminal_permit,
        _global_terminal_permit: global_terminal_permit,
    };
    let response_type = if s.0.config.single_origin {
        Some(
            if path
                .split('?')
                .next()
                .is_some_and(|p| p.ends_with("/live/events"))
            {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
    } else {
        None
    };
    let body = tokio::time::timeout(
        Duration::from_secs(10),
        axum::body::to_bytes(req.into_body(), protocol::MAX_HTTP_BODY),
    )
    .await
    .map_err(|_| Failure(StatusCode::REQUEST_TIMEOUT))?
    .map_err(|_| Failure(StatusCode::PAYLOAD_TOO_LARGE))?;
    let (attachment, receiver) = oneshot::channel();
    let (cancel, mut stop) = watch::channel(false);
    let request_headers = filtered_request(&h)?;
    {
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let c = tunnels
            .get_mut(&gateway)
            .ok_or(Failure(StatusCode::SERVICE_UNAVAILABLE))?;
        if c.epoch != admitted_epoch {
            return Err(Failure(StatusCode::SERVICE_UNAVAILABLE));
        }
        if c.workbench_api_version != protocol::WORKBENCH_API_VERSION
            || s.0.store.gateway_auth(&c.credential).is_none()
        {
            return Err(denied());
        }
        let mut streams = s.0.streams.lock().unwrap();
        if !s.0.store.active(session.id) {
            return Err(denied());
        }
        if streams.values().filter(|v| v.gateway == gateway).count() >= MAX_STREAMS {
            return Err(Failure(StatusCode::TOO_MANY_REQUESTS));
        }
        if c.sessions.len() >= 256 && !c.sessions.contains(&session.id) {
            return Err(Failure(StatusCode::TOO_MANY_REQUESTS));
        }
        c.sessions.insert(session.id);
        streams.insert(
            id,
            Stream {
                gateway,
                epoch: c.epoch,
                session: session.id,
                terminal,
                cancel,
                attach: Some(attachment),
            },
        );
        let request = Open {
            stream_id: id,
            session_id: session.id,
            instance_id: c.instance,
            kind: if terminal {
                StreamKind::WebSocket
            } else {
                StreamKind::Http
            },
            method,
            path,
            headers: request_headers,
        };
        if c.send.try_send(ToGateway::Open { request }).is_err() {
            streams.remove(&id);
            return Err(Failure(StatusCode::SERVICE_UNAVAILABLE));
        }
    }
    let mut data = tokio::select! {
        _=stop.changed()=>return Err(Failure(StatusCode::SERVICE_UNAVAILABLE)),
        result=tokio::time::timeout(Duration::from_secs(10),receiver)=>result.map_err(|_|Failure(StatusCode::GATEWAY_TIMEOUT))?.map_err(|_|Failure(StatusCode::BAD_GATEWAY))?
    };
    if let Some(upgrade) = ws {
        match next_data(&mut data, &mut stop).await? {
            Data::WebSocketReady => {}
            _ => return Err(Failure(StatusCode::BAD_GATEWAY)),
        }
        return Ok(upgrade
            .max_message_size(96 * 1024)
            .max_frame_size(96 * 1024)
            .on_upgrade(move |public| bridge(public, data, stop, guard)));
    }
    for chunk in body.chunks(MAX_FRAME) {
        tokio::select! {_=stop.changed()=>return Err(denied()), r=tokio::time::timeout(Duration::from_secs(10),data.send(Message::Binary(Bytes::copy_from_slice(chunk))))=>{r.map_err(|_|Failure(StatusCode::GATEWAY_TIMEOUT))?.map_err(|_|Failure(StatusCode::BAD_GATEWAY))?;}}
    }
    tokio::select! {_=stop.changed()=>return Err(denied()),r=send_json(&mut data,&Data::RequestEnd)=>r.map_err(|_|Failure(StatusCode::BAD_GATEWAY))?};
    let Data::ResponseHead {
        status,
        mut headers,
    } = next_data(&mut data, &mut stop).await?
    else {
        return Err(Failure(StatusCode::BAD_GATEWAY));
    };
    let status = StatusCode::from_u16(status)
        .ok()
        .filter(|s| s.as_u16() >= 200)
        .ok_or(Failure(StatusCode::BAD_GATEWAY))?;
    let mut response = Response::builder().status(status);
    let empty = matches!(status, StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED);
    if empty
        && let Some(expected) = response_type
        && !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), expected.into()));
    }
    for (key, value) in filtered_response(headers, response_type)? {
        response = response.header(key, value);
    }
    if empty {
        return response
            .body(Body::empty())
            .map_err(|_| Failure(StatusCode::BAD_GATEWAY));
    }
    let stream = async_stream::try_stream! {
        let _guard=guard;
        loop {
            let message=tokio::select!{_=stop.changed()=>None,m=data.recv()=>m};
            match message {
                Some(Ok(Message::Binary(bytes))) if bytes.len()<=MAX_FRAME=>yield bytes,
                Some(Ok(Message::Text(text))) if matches!(serde_json::from_str::<Data>(&text),Ok(Data::ResponseEnd))=>break,
                Some(Ok(Message::Ping(_)|Message::Pong(_)))=>continue,
                _=>{Err(std::io::Error::other("relay stream interrupted"))?;break;}
            }
        }
    };
    let stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = std::result::Result<Bytes, std::io::Error>> + Send>,
    > = Box::pin(stream);
    response
        .body(Body::from_stream(stream))
        .map_err(|_| Failure(StatusCode::BAD_GATEWAY))
}
async fn next_data(ws: &mut WebSocket, stop: &mut watch::Receiver<bool>) -> Result<Data> {
    tokio::select! {_=stop.changed()=>Err(denied()),r=tokio::time::timeout(Duration::from_secs(30),ws.recv())=>{
        match r {Ok(Some(Ok(Message::Text(text)))) if text.len()<=MAX_FRAME=>serde_json::from_str(&text).map_err(|_|Failure(StatusCode::BAD_GATEWAY)),_=>Err(Failure(StatusCode::BAD_GATEWAY))}
    }}
}
async fn bridge(
    mut public: WebSocket,
    mut agent: WebSocket,
    mut stop: watch::Receiver<bool>,
    _guard: StreamGuard,
) {
    loop {
        tokio::select! {
            _=stop.changed()=>break,
            msg=public.recv()=>match msg {
                Some(Ok(message @ (Message::Text(_)|Message::Binary(_))))=>if tokio::select!{_=stop.changed()=>true,r=tokio::time::timeout(Duration::from_secs(10),agent.send(message))=>!matches!(r,Ok(Ok(())))}{break;},
                Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},_=>break
            },
            msg=agent.recv()=>match msg {
                Some(Ok(message @ (Message::Text(_)|Message::Binary(_))))=>if tokio::select!{_=stop.changed()=>true,r=tokio::time::timeout(Duration::from_secs(10),public.send(message))=>!matches!(r,Ok(Ok(())))}{break;},
                Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},_=>break
            }
        }
    }
    // Dropping both sockets terminates without replay or terminal ACK synthesis.
}
fn filtered_request(h: &HeaderMap) -> Result<Vec<(String, String)>> {
    let mut output = Vec::new();
    for name in [
        "origin",
        "accept",
        "content-type",
        "if-match",
        "user-agent",
        "if-none-match",
        "if-modified-since",
        "last-event-id",
        "range",
    ] {
        for value in h.get_all(name) {
            output.push((name.to_string(), value.to_str().map_err(|_| bad())?.into()));
        }
    }
    Ok(output)
}
fn filtered_response(
    headers: Vec<(String, String)>,
    expected_type: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let mut names = HashSet::new();
    for (name, value) in &headers {
        if !names.insert(name.to_ascii_lowercase())
            || axum::http::HeaderName::try_from(name).is_err()
            || axum::http::HeaderValue::try_from(value).is_err()
            || value.len() >= 16384
        {
            return Err(Failure(StatusCode::BAD_GATEWAY));
        }
    }
    if let Some(expected) = expected_type {
        let supplied = headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.split(';').next().unwrap_or("").trim());
        if supplied.is_none_or(|v| !v.eq_ignore_ascii_case(expected)) {
            return Err(Failure(StatusCode::BAD_GATEWAY));
        }
    }
    let mut output: Vec<_> = headers
        .into_iter()
        .filter(|(n, v)| {
            v.len() < 16384
                && matches!(
                    n.to_ascii_lowercase().as_str(),
                    "content-type"
                        | "etag"
                        | "last-modified"
                        | "content-range"
                        | "accept-ranges"
                        | "content-disposition"
                )
        })
        .collect();
    if let Some(expected) = expected_type {
        output.retain(|(n, _)| !n.eq_ignore_ascii_case("content-type"));
        output.push(("content-type".into(), format!("{expected}; charset=utf-8")));
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn organization_preconditions_keep_duplicates_and_reject_opaque_values() {
        use axum::http::HeaderValue;
        for name in ["if-match", "content-type"] {
            let mut headers = HeaderMap::new();
            headers.append(name, HeaderValue::from_static("first"));
            headers.append(name, HeaderValue::from_static("second"));
            headers.insert("authorization", HeaderValue::from_static("private"));
            let forwarded = filtered_request(&headers).unwrap();
            assert_eq!(
                forwarded,
                vec![
                    (name.into(), "first".into()),
                    (name.into(), "second".into())
                ]
            );
            headers.append(name, HeaderValue::from_bytes(b"\xff").unwrap());
            assert!(matches!(
                filtered_request(&headers),
                Err(Failure(StatusCode::BAD_REQUEST))
            ));
        }
    }
    #[test]
    fn response_cannot_set_credentials_redirect_or_hop_headers() {
        let h = filtered_response(
            vec![
                ("Set-Cookie".into(), "owner=secret".into()),
                ("Location".into(), "https://evil.test".into()),
                ("Connection".into(), "upgrade".into()),
                ("Content-Type".into(), "text/event-stream".into()),
            ],
            None,
        )
        .unwrap();
        assert_eq!(h, vec![("Content-Type".into(), "text/event-stream".into())]);
    }
    #[test]
    fn same_origin_response_type_is_fixed_and_duplicate_headers_fail_closed() {
        for malicious in [
            "text/html",
            "text/javascript",
            "image/svg+xml",
            "application/json, text/html",
        ] {
            assert!(
                filtered_response(
                    vec![("content-type".into(), malicious.into())],
                    Some("application/json")
                )
                .is_err()
            );
        }
        assert!(
            filtered_response(
                vec![
                    ("content-type".into(), "application/json".into()),
                    ("Content-Type".into(), "text/html".into())
                ],
                Some("application/json")
            )
            .is_err()
        );
        assert!(
            filtered_response(
                vec![(
                    "content-type".into(),
                    "application/json\r\nX-Injected: yes".into()
                )],
                Some("application/json")
            )
            .is_err()
        );
        assert!(filtered_response(vec![], Some("application/json")).is_err());
        let safe = filtered_response(
            vec![
                (
                    "content-type".into(),
                    "application/json; charset=iso-8859-1".into(),
                ),
                ("Content-Security-Policy".into(), "default-src *".into()),
                ("Refresh".into(), "0;url=https://evil.test".into()),
                ("Link".into(), "</evil.js>; rel=preload".into()),
            ],
            Some("application/json"),
        )
        .unwrap();
        assert_eq!(
            safe,
            vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into()
            )]
        );
    }
}
