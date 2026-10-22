//! Endpoint multiplexing. Only this connector decrypts; the Relay is an opaque bridge.
use super::{
    crypto::{Cipher, Ephemeral},
    storage::Saved,
    transport::{self, Socket},
};
use crate::workbench::{permission::Permission, remote_access::IngressHandle};
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use codex_relay_protocol::{
    self as protocol,
    secure::{self, ChannelBinding, Frame, Handshake, Kind, Scope},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinSet,
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::{
    Message, client::IntoClientRequest, protocol::WebSocketConfig,
};

pub(super) struct Limits {
    streams: Arc<Semaphore>,
    terminals: Arc<Semaphore>,
    memory: Arc<Semaphore>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            streams: Arc::new(Semaphore::new(64)),
            terminals: Arc::new(Semaphore::new(4)),
            memory: Arc::new(Semaphore::new(secure::ENDPOINT_BUDGET)),
        }
    }
}
fn reserve(pool: &Arc<Semaphore>, n: usize) -> Result<OwnedSemaphorePermit> {
    pool.clone()
        .try_acquire_many_owned(n.try_into()?)
        .map_err(|_| anyhow::anyhow!("transport_budget"))
}
struct Packet {
    frame: Frame,
    priority: bool,
    _queue: OwnedSemaphorePermit,
    _memory: Option<OwnedSemaphorePermit>,
    _record: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct Output {
    tx: mpsc::Sender<Packet>,
    data: Arc<Semaphore>,
    control: Arc<Semaphore>,
    memory: Arc<Semaphore>,
    records: Arc<Semaphore>,
    control_records: Arc<Semaphore>,
    terminal: bool,
}
impl Output {
    async fn send(&self, frame: Frame) -> Result<()> {
        let control = matches!(
            frame,
            Frame::Window { .. }
                | Frame::Reset { .. }
                | Frame::Accepted { .. }
                | Frame::TerminalReady { .. }
        );
        let priority = self.terminal
            || control
            || matches!(frame, Frame::MessageStart { .. } | Frame::MessageEnd { .. });
        let n = serde_json::to_vec(&frame)?.len() + 128;
        ensure!(n <= secure::RECORD, "record_size");
        // The frame already owns its payload: account for it before waiting for
        // either queue bytes or a record slot, and retain that charge on cancel.
        let memory = if control {
            None
        } else {
            Some(reserve(&self.memory, n)?)
        };
        let record = if control {
            reserve(&self.control_records, 1)?
        } else {
            self.records.clone().acquire_owned().await?
        };
        let queue = if control {
            reserve(&self.control, n)?
        } else {
            self.data.clone().acquire_many_owned(n as u32).await?
        };
        self.tx
            .send(Packet {
                frame,
                priority,
                _queue: queue,
                _memory: memory,
                _record: record,
            })
            .await
            .map_err(|_| anyhow::anyhow!("channel_closed"))
    }
    async fn reset(&self, id: u32) -> Result<()> {
        self.send(Frame::Reset {
            id,
            code: "substream_failed".into(),
        })
        .await
    }
}
fn id(frame: &Frame) -> u32 {
    match frame {
        Frame::Finished => 0,
        Frame::Open { id, .. }
        | Frame::Accepted { id, .. }
        | Frame::Head { id, .. }
        | Frame::Chunk { id, .. }
        | Frame::End { id }
        | Frame::TerminalReady { id }
        | Frame::MessageStart { id, .. }
        | Frame::MessageEnd { id }
        | Frame::Window { id, .. }
        | Frame::Reset { id, .. } => *id,
    }
}
fn next_packet(
    queues: &mut BTreeMap<u32, VecDeque<Packet>>,
    last: &mut [u32; 2],
    high: &mut u32,
) -> Packet {
    let has_high = queues
        .values()
        .any(|q| q.front().is_some_and(|p| p.priority));
    let has_normal = queues
        .values()
        .any(|q| q.front().is_some_and(|p| !p.priority));
    let priority = has_high && (*high < 4 || !has_normal);
    // Each class keeps its own round-robin cursor. Otherwise a busy priority
    // stream can repeatedly move the cursor past an ordinary stream, starving
    // that stream even while the burst limit regularly selects normal traffic.
    let cursor = &mut last[usize::from(priority)];
    let eligible = |q: &&VecDeque<Packet>| q.front().is_some_and(|p| p.priority == priority);
    let chosen = queues
        .range((
            std::ops::Bound::Excluded(*cursor),
            std::ops::Bound::Unbounded,
        ))
        .filter(|(_, q)| eligible(q))
        .map(|(k, _)| *k)
        .next()
        .or_else(|| {
            queues
                .iter()
                .filter(|(_, q)| eligible(q))
                .map(|(k, _)| *k)
                .next()
        })
        .unwrap();
    let q = queues.get_mut(&chosen).unwrap();
    let packet = q.pop_front().unwrap();
    if q.is_empty() {
        queues.remove(&chosen);
    }
    *cursor = chosen;
    if priority {
        *high = high.saturating_add(1);
    } else {
        *high = 0;
    }
    packet
}
async fn writer(
    mut socket: futures_util::stream::SplitSink<Socket, Message>,
    mut cipher: Cipher,
    mut incoming: mpsc::Receiver<Packet>,
) -> Result<()> {
    let mut queues = BTreeMap::<u32, VecDeque<Packet>>::new();
    let mut last = [0; 2];
    let mut high = 0;
    loop {
        if queues.is_empty() {
            let Some(p) = incoming.recv().await else {
                return Ok(());
            };
            queues.entry(id(&p.frame)).or_default().push_back(p);
        }
        // One bounded incoming queue plus bounded per-stream queues (byte permits
        // live until actual send); priority never overtakes a stream's own FIFO.
        while let Ok(p) = incoming.try_recv() {
            queues.entry(id(&p.frame)).or_default().push_back(p);
        }
        let packet = next_packet(&mut queues, &mut last, &mut high);
        // Once seal assigns the global counter, send or fail the entire channel.
        let bytes = cipher.seal(&packet.frame)?;
        timeout(
            Duration::from_secs(10),
            socket.send(Message::Binary(bytes.into())),
        )
        .await??;
    }
}
struct Flow {
    credit: Mutex<usize>,
    wake: Notify,
    stop: Permission,
}
impl Flow {
    fn new(credit: usize, stop: Permission) -> Self {
        Self {
            credit: Mutex::new(credit),
            wake: Notify::new(),
            stop,
        }
    }
    fn add(&self, n: usize) -> Result<()> {
        let mut v = self.credit.lock().unwrap();
        ensure!(
            n > 0 && n <= secure::WINDOW && *v + n <= secure::WINDOW,
            "window_limit"
        );
        *v += n;
        self.wake.notify_one();
        Ok(())
    }
    async fn take(&self, n: usize) -> Result<()> {
        loop {
            let wake = self.wake.notified();
            {
                let mut v = self.credit.lock().unwrap();
                if *v >= n {
                    *v -= n;
                    return Ok(());
                }
            }
            tokio::select! {_=self.stop.revoked()=>bail!("substream_closed"),_=wake=>{}}
        }
    }
    fn empty(&self) -> bool {
        *self.credit.lock().unwrap() == 0
    }
}
struct Assembly {
    bytes: Vec<u8>,
    length: usize,
    binary: bool,
    credit: usize,
    _memory: OwnedSemaphorePermit,
}
impl Assembly {
    fn new(length: usize, binary: bool, memory: &Arc<Semaphore>) -> Result<Self> {
        let permit = reserve(memory, length)?;
        Ok(Self {
            bytes: Vec::with_capacity(length),
            length,
            binary,
            credit: 0,
            _memory: permit,
        })
    }
    fn window(&mut self) -> usize {
        let n = (self.length - self.bytes.len() - self.credit).min(secure::WINDOW - self.credit);
        self.credit += n;
        n
    }
    fn chunk(&mut self, data: &str) -> Result<()> {
        ensure!(data.len() <= 4 * secure::CHUNK.div_ceil(3), "chunk_size");
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| anyhow::anyhow!("chunk_encoding"))?;
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= secure::CHUNK
                && bytes.len() <= self.credit
                && self.bytes.len() + bytes.len() <= self.length,
            "chunk_window"
        );
        self.credit -= bytes.len();
        self.bytes.extend(bytes);
        Ok(())
    }
    fn complete(&self) -> bool {
        self.bytes.len() == self.length
    }
}
struct StreamResources {
    _permit: OwnedSemaphorePermit,
    _terminal: Option<OwnedSemaphorePermit>,
    _metadata: OwnedSemaphorePermit,
}
impl StreamResources {
    fn reserve(limits: &Limits, terminal: bool) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            _permit: limits.streams.clone().try_acquire_owned()?,
            _terminal: if terminal {
                Some(limits.terminals.clone().try_acquire_owned()?)
            } else {
                None
            },
            // One bounded Open record and its sanitized request-header map.
            // The serial reader's temporary conversion is covered by _fixed.
            _metadata: reserve(&limits.memory, secure::RECORD + 8 * 1024)?,
        }))
    }
}
struct Stream {
    kind: Kind,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    request: Option<Assembly>,
    message: Option<Assembly>,
    input: Option<mpsc::Sender<Assembly>>,
    flow: Arc<Flow>,
    stop: Permission,
    started: bool,
    deadline: Instant,
    resources: Arc<StreamResources>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.revoke();
    }
}
#[derive(Clone)]
struct Local {
    address: std::net::SocketAddr,
    origin: String,
    capability: String,
}
impl Local {
    fn headers(&self, headers: &[(String, String)]) -> Result<reqwest::header::HeaderMap> {
        ensure!(
            headers.len() <= 64
                && headers
                    .iter()
                    .map(|(n, v)| n.len() + v.len())
                    .sum::<usize>()
                    <= 32 * 1024,
            "headers_limit"
        );
        for (n, v) in headers {
            reqwest::header::HeaderName::from_bytes(n.as_bytes())?;
            reqwest::header::HeaderValue::from_str(v)?;
        }
        let mut h = transport::request_headers(headers);
        h.insert(
            "host",
            self.origin
                .split_once("://")
                .ok_or_else(|| anyhow::anyhow!("origin"))?
                .1
                .parse()?,
        );
        h.insert("origin", self.origin.parse()?);
        h.insert(
            "authorization",
            format!("Bearer {}", self.capability).parse()?,
        );
        Ok(h)
    }
}

pub(super) async fn run(
    saved: &Saved,
    ingress: IngressHandle,
    binding: ChannelBinding,
    capability: String,
    limits: Arc<Limits>,
) -> Result<()> {
    let bound = saved
        .bound
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("unbound"))?;
    ensure!(binding.valid(), "binding");
    let permission = ingress
        .permission(binding.context.grant_id)
        .ok_or_else(|| anyhow::anyhow!("grant_revoked"))?;
    let life = match binding.scope {
        Scope::Application => Permission::new(),
        Scope::Run { run_id } => ingress
            .resolve_run(run_id)
            .ok_or_else(|| anyhow::anyhow!("run_retired"))?,
    };
    // Includes independent reader/writer AEAD, JSON serialization and parsing
    // scratch. Queued frames and retained whole messages are charged separately.
    let _fixed = reserve(&limits.memory, secure::CONTROL_RESERVE + 6 * secure::RECORD)?;
    let mut socket = transport::connect(
        transport::ws_url(
            &saved.server_url,
            &format!(
                "/relay/v1/tunnel/channels/{}?epoch={}",
                binding.channel_id, binding.context.connection_epoch
            ),
        )?,
        &bound.credential,
        secure::WIRE_RECORD,
    )
    .await?;
    let handshake = timeout(Duration::from_secs(10), async {
        let Some(Ok(Message::Text(text))) = socket.next().await else {
            bail!("client_hello_required");
        };
        ensure!(text.len() <= secure::HANDSHAKE, "handshake_size");
        let Handshake::ClientHello { public_key } =
            serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("client_hello"))?
        else {
            bail!("client_hello_required");
        };
        let key = Ephemeral::new()?;
        transport::json(
            &mut socket,
            &Handshake::ServerHello {
                public_key: key.public.clone(),
            },
        )
        .await?;
        let (mut send, mut receive) = key.agree(&binding, &public_key, true)?;
        let Some(Ok(Message::Binary(bytes))) = socket.next().await else {
            bail!("finished_required");
        };
        receive.finished(&bytes)?;
        ensure!(permission.active() && life.active(), "scope_revoked");
        transport::send(
            &mut socket,
            Message::Binary(send.seal(&Frame::Finished)?.into()),
        )
        .await?;
        Result::<_>::Ok((send, receive))
    });
    let (send, mut receive) = tokio::select! {_=permission.revoked()=>bail!("grant_revoked"),_=life.revoked()=>bail!("run_retired"),r=handshake=>r??};
    ensure!(permission.active() && life.active(), "scope_revoked");
    let local = Local {
        address: ingress.address(),
        origin: bound.gateway_origin.clone(),
        capability,
    };
    ensure!(local.address.ip().is_loopback(), "private_ingress");
    // Every queued/in-flight packet holds one of 128 data or 64 control record
    // permits. 192 slots therefore guarantee the reader never waits for an
    // item slot after acquiring a control permit (even with a blocked writer).
    let (sink, mut reader) = socket.split();
    let (tx, rx) = mpsc::channel(192);
    let out = Output {
        tx,
        data: Arc::new(Semaphore::new(secure::CHANNEL_QUEUE)),
        control: Arc::new(Semaphore::new(secure::CONTROL_RESERVE)),
        memory: limits.memory.clone(),
        records: Arc::new(Semaphore::new(128)),
        control_records: Arc::new(Semaphore::new(64)),
        terminal: false,
    };
    let mut writing = tokio::spawn(writer(sink, send, rx));
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _writer = Abort(writing.abort_handle());
    let mut tasks = JoinSet::<(u32, Result<()>)>::new();
    let mut streams = BTreeMap::<u32, Stream>::new();
    let mut highest = 0;
    let mut sweep = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            biased;
            _=permission.revoked()=>bail!("grant_revoked"),
            _=life.revoked()=>bail!("run_retired"),
            done=&mut writing=>{done??;bail!("writer_closed");},
            _=sweep.tick()=> {
                ensure!(permission.active()&&life.active(),"scope_revoked");
                let expired:Vec<_>=streams.iter().filter(|(_,s)|!s.started&&Instant::now()>s.deadline).map(|(id,_)|*id).collect();
                for id in expired {streams.remove(&id);out.reset(id).await?;}
            },
            result=tasks.join_next(),if !tasks.is_empty()=> {
                if let Some(Ok((id,result)))=result {
                    // A Reset or a rejected frame may already have removed this
                    // child and aborted its worker. Its late completion cannot
                    // send a second Reset or act on a subsequent child.
                    if streams.remove(&id).is_some()&&result.is_err(){out.reset(id).await?;}
                }
            },
            incoming=reader.next()=> {
                ensure!(permission.active()&&life.active(),"scope_revoked");
                let frame=match incoming {
                    Some(Ok(Message::Binary(bytes)))=>receive.open(&bytes)?,
                    Some(Ok(Message::Ping(_)|Message::Pong(_)))=>continue,
                    _=>bail!("channel_interrupted"),
                };
                let stream_id=id(&frame);
                if matches!(frame,Frame::Open{..}) {
                    ensure!(stream_id>highest,"stream_sequence");highest=stream_id;
                    match open(frame,&binding,&ingress,&limits) {
                        Ok(mut stream)=>{let credit=stream.request.as_mut().unwrap().window();streams.insert(stream_id,stream);out.send(Frame::Accepted{id:stream_id,credit}).await?;},
                        Err(_)=>out.reset(stream_id).await?,
                    }
                    continue;
                }
                // Late records for closed ids still passed AEAD and advanced its
                // global sequence. They cannot resurrect state or allocations.
                if !streams.contains_key(&stream_id) {ensure!(stream_id>0&&stream_id<=highest,"unknown_stream");continue;}
                if matches!(frame,Frame::Reset{..}) {streams.remove(&stream_id);continue;}
                let stream=streams.get_mut(&stream_id).unwrap();
                let result=handle(frame,stream,&limits,&out,&local,&permission,&life,&mut tasks).await;
                if result.is_err(){streams.remove(&stream_id);out.reset(stream_id).await?;}
            }
        }
    }
}

fn open(
    frame: Frame,
    binding: &ChannelBinding,
    ingress: &IngressHandle,
    limits: &Limits,
) -> Result<Stream> {
    let Frame::Open {
        kind,
        method,
        path,
        headers,
        body_length,
        receive_window,
        ..
    } = frame
    else {
        bail!("open_required");
    };
    ensure!(
        secure::request_scope(&method, &path).as_ref() == Some(&binding.scope),
        "scope_mismatch"
    );
    if let Scope::Run { run_id } = binding.scope {
        ensure!(ingress.resolve_run(run_id).is_some(), "run_retired");
    }
    ensure!(
        body_length <= protocol::MAX_HTTP_BODY && receive_window <= secure::WINDOW,
        "open_limits"
    );
    if kind == Kind::Terminal {
        ensure!(
            method == "GET"
                && path.split('?').next().unwrap_or("").ends_with("/terminal")
                && body_length == 0
                && receive_window == 0,
            "terminal_open"
        );
    } else {
        ensure!(
            !path.split('?').next().unwrap_or("").ends_with("/terminal"),
            "terminal_kind"
        );
    }
    ensure!(
        headers.len() <= 64
            && headers
                .iter()
                .map(|(n, v)| n.len() + v.len())
                .sum::<usize>()
                <= 32 * 1024,
        "headers_limit"
    );
    let resources = StreamResources::reserve(limits, kind == Kind::Terminal)?;
    let request = Assembly::new(body_length, false, &limits.memory)?;
    let stop = Permission::new();
    Ok(Stream {
        kind,
        method,
        path,
        headers,
        request: Some(request),
        message: None,
        input: None,
        flow: Arc::new(Flow::new(receive_window, stop.clone())),
        stop,
        started: false,
        deadline: Instant::now() + Duration::from_secs(10),
        resources,
    })
}

#[allow(clippy::too_many_arguments)]
async fn handle(
    frame: Frame,
    stream: &mut Stream,
    limits: &Limits,
    out: &Output,
    local: &Local,
    permission: &Permission,
    life: &Permission,
    tasks: &mut JoinSet<(u32, Result<()>)>,
) -> Result<()> {
    let stream_id = id(&frame);
    match frame {
        Frame::Window { credit, .. } => stream.flow.add(credit)?,
        Frame::Chunk { data, .. } => {
            let assembler = if !stream.started {
                stream.request.as_mut()
            } else if stream.kind == Kind::Terminal {
                stream.message.as_mut()
            } else {
                None
            };
            let assembler = assembler.ok_or_else(|| anyhow::anyhow!("unexpected_chunk"))?;
            assembler.chunk(&data)?;
            let credit = assembler.window();
            if credit > 0 {
                out.send(Frame::Window {
                    id: stream_id,
                    credit,
                })
                .await?;
            }
        }
        Frame::End { .. } => {
            ensure!(!stream.started, "duplicate_end");
            let body = stream.request.take().unwrap();
            ensure!(body.complete(), "incomplete_request");
            ensure!(permission.active() && life.active(), "scope_revoked");
            stream.started = true;
            let local = local.clone();
            let mut output = out.clone();
            output.terminal = stream.kind == Kind::Terminal;
            let flow = stream.flow.clone();
            let stop = stream.stop.clone();
            let headers = local.headers(&stream.headers)?;
            stream.headers.clear();
            let path = std::mem::take(&mut stream.path);
            let method = std::mem::take(&mut stream.method);
            // A Reset removes Stream before its worker is actually dropped.
            // Keep both metadata and quota permits charged until that drop.
            let resources = stream.resources.clone();
            let worker = if stream.kind == Kind::Http {
                tokio::spawn(async move {
                    let _resources = resources;
                    http(stream_id, local, method, path, headers, body, output, flow).await
                })
            } else {
                let (tx, rx) = mpsc::channel(4);
                stream.input = Some(tx);
                tokio::spawn(async move {
                    let _resources = resources;
                    terminal(stream_id, local, path, headers, rx, output, flow).await
                })
            };
            // The join wrapper owns abort-on-drop: channel teardown/Reset cannot
            // leave a detached local mutation/terminal forwarding task behind.
            tasks.spawn(async move {
                struct Abort(tokio::task::AbortHandle);impl Drop for Abort {fn drop(&mut self){self.0.abort();}}
                let _abort=Abort(worker.abort_handle());
                let result=tokio::select!{_=stop.revoked()=>Err(anyhow::anyhow!("substream_closed")),r=worker=>match r {Ok(r)=>r,Err(_)=>Err(anyhow::anyhow!("substream_task"))}};
                (stream_id,result)
            });
        }
        Frame::MessageStart { length, binary, .. } => {
            ensure!(
                stream.started
                    && stream.kind == Kind::Terminal
                    && stream.message.is_none()
                    && length <= 96 * 1024,
                "message_start"
            );
            let mut message = Assembly::new(length, binary, &limits.memory)?;
            let credit = message.window();
            stream.message = Some(message);
            if credit > 0 {
                out.send(Frame::Window {
                    id: stream_id,
                    credit,
                })
                .await?;
            }
        }
        Frame::MessageEnd { .. } => {
            let message = stream
                .message
                .take()
                .ok_or_else(|| anyhow::anyhow!("message_start_required"))?;
            ensure!(message.complete(), "incomplete_message");
            stream
                .input
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("terminal_not_ready"))?
                .try_send(message)
                .map_err(|_| anyhow::anyhow!("slow_terminal"))?;
        }
        _ => bail!("unexpected_frame"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn http(
    id: u32,
    local: Local,
    method: String,
    path: String,
    headers: reqwest::header::HeaderMap,
    body: Assembly,
    out: Output,
    flow: Arc<Flow>,
) -> Result<()> {
    let Assembly {
        bytes,
        _memory: reservation,
        ..
    } = body;
    let response = timeout(
        Duration::from_secs(10),
        transport::local_client()?
            .request(method.parse()?, format!("http://{}{}", local.address, path))
            .headers(headers)
            .body(bytes)
            .send(),
    )
    .await??;
    drop(reservation);
    let status = response.status().as_u16();
    ensure!(
        !(300..400).contains(&status) || status == 304,
        "redirect_rejected"
    );
    let expected = if path
        .split('?')
        .next()
        .unwrap_or("")
        .ends_with("/live/events")
    {
        "text/event-stream"
    } else {
        "application/json"
    };
    let headers = safe_response_headers(response.headers(), expected, matches!(status, 204 | 304))?;
    out.send(Frame::Head {
        id,
        status,
        headers,
    })
    .await?;
    let mut source = response.bytes_stream();
    while let Some(chunk) = source.next().await {
        // Copy individual bounded fragments rather than pinning a large backing
        // Bytes allocation in every queue entry. Source remains independently bounded.
        for part in chunk?.chunks(secure::CHUNK) {
            flow.take(part.len()).await?;
            out.send(Frame::Chunk {
                id,
                data: STANDARD.encode(part),
            })
            .await?;
        }
    }
    out.send(Frame::End { id }).await
}

async fn terminal(
    id: u32,
    local: Local,
    path: String,
    headers: reqwest::header::HeaderMap,
    mut input: mpsc::Receiver<Assembly>,
    out: Output,
    flow: Arc<Flow>,
) -> Result<()> {
    let mut request = format!("ws://{}{}", local.address, path).into_client_request()?;
    for (n, v) in &headers {
        request.headers_mut().insert(n.clone(), v.clone());
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(protocol::MAX_WS_MESSAGE))
        .max_frame_size(Some(protocol::MAX_WS_MESSAGE))
        .write_buffer_size(0);
    let (socket, _) = timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await??;
    out.send(Frame::TerminalReady { id }).await?;
    let (mut sink, mut source) = socket.split();
    let incoming = async {
        while let Some(message) = input.recv().await {
            let Assembly {
                bytes,
                binary,
                _memory: reservation,
                ..
            } = message;
            let wire = if binary {
                Message::Binary(bytes.into())
            } else {
                Message::Text(
                    String::from_utf8(bytes)
                        .map_err(|_| anyhow::anyhow!("terminal_utf8"))?
                        .into(),
                )
            };
            timeout(Duration::from_secs(10), sink.send(wire)).await??;
            drop(reservation);
        }
        Result::<()>::Err(anyhow::anyhow!("terminal_input_closed"))
    };
    let outgoing = async {
        while let Some(message) = source.next().await {
            let message = message?;
            if matches!(message, Message::Ping(_) | Message::Pong(_)) {
                continue;
            }
            if !matches!(message, Message::Text(_) | Message::Binary(_)) {
                break;
            }
            let (bytes, binary, _reservation) = terminal_message(message, &out.memory)?;
            ensure!(flow.empty(), "terminal_message_limit");
            out.send(Frame::MessageStart {
                id,
                length: bytes.len(),
                binary,
            })
            .await?;
            for part in bytes.chunks(secure::CHUNK) {
                flow.take(part.len()).await?;
                out.send(Frame::Chunk {
                    id,
                    data: STANDARD.encode(part),
                })
                .await?;
            }
            out.send(Frame::MessageEnd { id }).await?;
        }
        out.send(Frame::End { id }).await
    };
    tokio::select! {r=incoming=>r,r=outgoing=>r}
}

fn terminal_message(
    message: Message,
    memory: &Arc<Semaphore>,
) -> Result<(axum::body::Bytes, bool, OwnedSemaphorePermit)> {
    let binary = matches!(message, Message::Binary(_));
    let bytes = message.into_data();
    ensure!(
        bytes.len() <= protocol::MAX_WS_MESSAGE,
        "terminal_message_limit"
    );
    // The native WS collector has its own 16 MiB bound. On transfer into the
    // multiplexer's credit wait, charge the entire retained buffer immediately;
    // budget exhaustion resets only this terminal, never waits for expansion.
    let reservation = reserve(memory, bytes.len())?;
    Ok((bytes, binary, reservation))
}

fn safe_response_headers(
    headers: &reqwest::header::HeaderMap,
    expected: &str,
    empty: bool,
) -> Result<Vec<(String, String)>> {
    let mut names = std::collections::HashSet::new();
    let mut result = Vec::new();
    for (name, value) in headers {
        ensure!(names.insert(name.as_str()), "duplicate_response_header");
        let value = value
            .to_str()
            .map_err(|_| anyhow::anyhow!("response_header"))?;
        ensure!(value.len() < 16 * 1024, "response_header_limit");
        if name == "content-type" {
            ensure!(
                value
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case(expected),
                "response_type"
            );
        } else if matches!(
            name.as_str(),
            "etag" | "last-modified" | "content-range" | "accept-ranges" | "content-disposition"
        ) {
            result.push((name.to_string(), value.to_string()));
        }
    }
    ensure!(empty || names.contains("content-type"), "response_type");
    result.push(("content-type".into(), format!("{expected}; charset=utf-8")));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> (Output, mpsc::Receiver<Packet>) {
        let (tx, rx) = mpsc::channel(192);
        (
            Output {
                tx,
                data: Arc::new(Semaphore::new(secure::CHANNEL_QUEUE)),
                control: Arc::new(Semaphore::new(secure::CONTROL_RESERVE)),
                memory: Arc::new(Semaphore::new(secure::ENDPOINT_BUDGET)),
                records: Arc::new(Semaphore::new(128)),
                control_records: Arc::new(Semaphore::new(64)),
                terminal: false,
            },
            rx,
        )
    }

    #[tokio::test]
    async fn full_data_queue_cannot_take_control_record_slots() {
        let (out, mut rx) = output();
        for id in 1..=128 {
            out.send(Frame::End { id }).await.unwrap();
        }
        for id in 1..=64 {
            timeout(
                Duration::from_millis(100),
                out.send(Frame::Window { id, credit: 1 }),
            )
            .await
            .unwrap()
            .unwrap();
        }
        assert!(out.reset(1).await.is_err());
        assert_eq!(rx.len(), 192);
        while rx.try_recv().is_ok() {}
        assert_eq!(out.records.available_permits(), 128);
        assert_eq!(out.control_records.available_permits(), 64);
        assert_eq!(out.memory.available_permits(), secure::ENDPOINT_BUDGET);
    }

    #[tokio::test]
    async fn pending_output_is_budgeted_before_waiting_and_refunded_on_cancel() {
        let (out, _rx) = output();
        let _full = reserve(&out.data, secure::CHANNEL_QUEUE).unwrap();
        let frame = Frame::Chunk {
            id: 1,
            data: STANDARD.encode([0; secure::CHUNK]),
        };
        let bytes = serde_json::to_vec(&frame).unwrap().len() + 128;
        let mut pending = Box::pin(out.send(frame));
        assert!(
            timeout(Duration::from_millis(10), pending.as_mut())
                .await
                .is_err()
        );
        assert_eq!(
            out.memory.available_permits(),
            secure::ENDPOINT_BUDGET - bytes
        );
        drop(pending);
        assert_eq!(out.memory.available_permits(), secure::ENDPOINT_BUDGET);
        assert_eq!(out.records.available_permits(), 128);

        let _empty = reserve(&out.memory, secure::ENDPOINT_BUDGET).unwrap();
        assert!(
            timeout(Duration::from_millis(100), out.send(Frame::End { id: 1 }))
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn priority_burst_cannot_starve_normal_heads_and_preserves_stream_fifo() {
        let (out, mut rx) = output();
        out.send(Frame::End { id: 1 }).await.unwrap();
        for _ in 0..6 {
            for id in [2, 4, 6, 8, 10] {
                out.send(Frame::Window { id, credit: 1 }).await.unwrap();
            }
        }
        // This later high-priority record cannot overtake the normal head of id1.
        out.send(Frame::Window { id: 1, credit: 1 }).await.unwrap();
        let mut queues = BTreeMap::<u32, VecDeque<Packet>>::new();
        while let Ok(packet) = rx.try_recv() {
            queues
                .entry(id(&packet.frame))
                .or_default()
                .push_back(packet);
        }
        let (mut last, mut high) = ([0; 2], 0);
        for expected in [2, 4, 6, 8, 1] {
            assert_eq!(
                id(&next_packet(&mut queues, &mut last, &mut high).frame),
                expected
            );
        }
        assert_eq!(queues[&1].len(), 1);
        assert!(matches!(queues[&1][0].frame, Frame::Window { .. }));
    }

    #[tokio::test]
    async fn priority_traffic_cannot_starve_a_lower_numbered_normal_stream() {
        let (out, mut rx) = output();
        for _ in 0..4 {
            for id in [1, 3] {
                out.send(Frame::End { id }).await.unwrap();
            }
        }
        for _ in 0..16 {
            out.send(Frame::Window { id: 2, credit: 1 }).await.unwrap();
        }
        let mut queues = BTreeMap::<u32, VecDeque<Packet>>::new();
        while let Ok(packet) = rx.try_recv() {
            queues
                .entry(id(&packet.frame))
                .or_default()
                .push_back(packet);
        }
        let (mut last, mut high) = ([0; 2], 0);
        let mut normal = Vec::new();
        for _ in 0..20 {
            let packet = next_packet(&mut queues, &mut last, &mut high);
            if !packet.priority {
                normal.push(id(&packet.frame));
            }
        }
        assert_eq!(normal, [1, 3, 1, 3]);
    }

    #[test]
    fn shared_endpoint_limits_do_not_reset_while_old_generation_holds_permits() {
        let limits = Arc::new(Limits::default());
        let next = limits.clone();
        let old_streams = reserve(&limits.streams, 64).unwrap();
        let old_terminals = reserve(&limits.terminals, 4).unwrap();
        let old_bytes = reserve(&limits.memory, secure::ENDPOINT_BUDGET).unwrap();
        assert!(reserve(&next.streams, 1).is_err());
        assert!(reserve(&next.terminals, 1).is_err());
        assert!(reserve(&next.memory, 1).is_err());
        drop((old_streams, old_terminals, old_bytes));
        assert_eq!(next.streams.available_permits(), 64);
        assert_eq!(next.terminals.available_permits(), 4);
        assert_eq!(next.memory.available_permits(), secure::ENDPOINT_BUDGET);
    }

    #[test]
    fn stream_resources_remain_charged_until_cancelled_worker_is_dropped() {
        let limits = Limits::default();
        let stream = StreamResources::reserve(&limits, true).unwrap();
        let worker = stream.clone();
        drop(stream);
        assert_eq!(limits.streams.available_permits(), 63);
        assert_eq!(limits.terminals.available_permits(), 3);
        assert_eq!(
            limits.memory.available_permits(),
            secure::ENDPOINT_BUDGET - secure::RECORD - 8 * 1024
        );
        drop(worker);
        assert_eq!(limits.streams.available_permits(), 64);
        assert_eq!(limits.terminals.available_permits(), 4);
        assert_eq!(limits.memory.available_permits(), secure::ENDPOINT_BUDGET);

        let _full = reserve(&limits.memory, secure::ENDPOINT_BUDGET).unwrap();
        assert!(StreamResources::reserve(&limits, true).is_err());
        assert_eq!(limits.streams.available_permits(), 64);
        assert_eq!(limits.terminals.available_permits(), 4);
    }

    #[test]
    fn slow_terminal_messages_cannot_retain_unbudgeted_whole_buffers() {
        let pool = Arc::new(Semaphore::new(secure::ENDPOINT_BUDGET));
        let first =
            terminal_message(Message::Binary(vec![1; 16 * 1024 * 1024].into()), &pool).unwrap();
        let second =
            terminal_message(Message::Binary(vec![2; 16 * 1024 * 1024].into()), &pool).unwrap();
        assert_eq!(pool.available_permits(), 0);
        assert!(terminal_message(Message::Binary(vec![3].into()), &pool).is_err());
        drop(first);
        let third = terminal_message(Message::Text("synthetic".into()), &pool).unwrap();
        assert!(!third.1);
        drop((second, third));
        assert_eq!(pool.available_permits(), secure::ENDPOINT_BUDGET);
    }

    #[test]
    fn response_mime_is_fixed_and_duplicate_headers_fail_closed() {
        use reqwest::header::{HeaderMap, HeaderValue};
        for malicious in [
            "text/html",
            "text/javascript",
            "image/svg+xml",
            "application/json, text/html",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", malicious.parse().unwrap());
            assert!(safe_response_headers(&headers, "application/json", false).is_err());
        }
        let mut headers = HeaderMap::new();
        headers.append("content-type", HeaderValue::from_static("application/json"));
        headers.append("content-type", HeaderValue::from_static("text/html"));
        assert!(safe_response_headers(&headers, "application/json", false).is_err());
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/json; charset=iso-8859-1"),
        );
        headers.insert("set-cookie", HeaderValue::from_static("synthetic=private"));
        headers.insert("location", HeaderValue::from_static("https://evil.invalid"));
        assert_eq!(
            safe_response_headers(&headers, "application/json", false).unwrap(),
            vec![(
                "content-type".into(),
                "application/json; charset=utf-8".into()
            )]
        );
        assert!(safe_response_headers(&HeaderMap::new(), "application/json", false).is_err());
        assert!(safe_response_headers(&HeaderMap::new(), "application/json", true).is_ok());
    }
}
