//! Local-only transparent native TUI WebSocket tee. No RPC parsing or mutation.
//! Capture is a bounded, lossy observer; consumers must invalidate on any gap,
//! connection epoch change, close, or unsupported semantic evidence.
use axum::body::Bytes;
use futures_util::{
    SinkExt, StreamExt,
    stream::{FuturesUnordered, SplitSink, SplitStream},
};
use std::{
    fmt, io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async_with_config, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

const WS_LIMIT: usize = 128 * 1024 * 1024;
const HEADER_LIMIT: usize = 64 * 1024;
const HANDSHAKE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SafeFailure {
    InvalidLimits = 1,
    UnsafePath,
    SocketExists,
    Bind,
    Handshake,
    Transport,
    CaptureLimit,
    CaptureDropped,
    BinaryMessage,
    Stopped,
}
impl fmt::Display for SafeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native relay {self:?}")
    }
}
impl std::error::Error for SafeFailure {}
#[derive(Clone, Copy, Debug)]
pub struct RelayLimits {
    pub capture_bytes: usize,
    pub capture_frames: usize,
}
impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            capture_bytes: 8 * 1024 * 1024,
            capture_frames: 256,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ToServer,
    ToTui,
}
// Deliberately no Debug/Serialize: bytes are untrusted private protocol content.
pub struct Frame {
    pub connection_epoch: Uuid,
    pub sequence: u64,
    pub direction: Direction,
    pub bytes: Bytes,
    _permit: OwnedSemaphorePermit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureState {
    pub captured_sequence: u64,
    pub connection_epoch: Option<Uuid>,
    pub gap_generation: u64,
    pub closed: bool,
    pub reason: Option<SafeFailure>,
}
struct State {
    captured_sequence: AtomicU64,
    epoch_version: AtomicU64,
    epoch_hi: AtomicU64,
    epoch_lo: AtomicU64,
    gap: AtomicU64,
    closed: AtomicBool,
    stopped: AtomicBool,
    reason: AtomicU8,
    revision: AtomicU64,
    notify: Notify,
}
impl State {
    fn new() -> Self {
        Self {
            captured_sequence: AtomicU64::new(0),
            epoch_version: AtomicU64::new(0),
            epoch_hi: AtomicU64::new(0),
            epoch_lo: AtomicU64::new(0),
            gap: AtomicU64::new(0),
            closed: AtomicBool::new(true),
            stopped: AtomicBool::new(false),
            reason: AtomicU8::new(0),
            revision: AtomicU64::new(0),
            notify: Notify::new(),
        }
    }
    fn snapshot(&self) -> CaptureState {
        self.snapshot_with(|| {})
    }
    fn snapshot_with(&self, mut after_epoch_read: impl FnMut()) -> CaptureState {
        loop {
            let before = self.epoch_version.load(Ordering::Acquire);
            if !before.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let hi = self.epoch_hi.load(Ordering::Acquire);
            let lo = self.epoch_lo.load(Ordering::Acquire);
            after_epoch_read();
            let captured_sequence = self.captured_sequence.load(Ordering::Acquire);
            let gap_generation = self.gap.load(Ordering::Acquire);
            let closed = self.closed.load(Ordering::Acquire);
            let reason = match self.reason.load(Ordering::Acquire) {
                1 => Some(SafeFailure::InvalidLimits),
                2 => Some(SafeFailure::UnsafePath),
                3 => Some(SafeFailure::SocketExists),
                4 => Some(SafeFailure::Bind),
                5 => Some(SafeFailure::Handshake),
                6 => Some(SafeFailure::Transport),
                7 => Some(SafeFailure::CaptureLimit),
                8 => Some(SafeFailure::CaptureDropped),
                9 => Some(SafeFailure::BinaryMessage),
                10 => Some(SafeFailure::Stopped),
                _ => None,
            };
            // Include the connection state, not only UUID halves, in the seqlock.
            if self.epoch_version.load(Ordering::Acquire) != before {
                continue;
            }
            let stopped = self.stopped.load(Ordering::Acquire);
            return CaptureState {
                captured_sequence,
                connection_epoch: (before != 0).then(|| Uuid::from_u64_pair(hi, lo)),
                gap_generation,
                closed: stopped || closed,
                reason: if stopped {
                    Some(SafeFailure::Stopped)
                } else {
                    reason
                },
            };
        }
    }
    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
        self.notify.notify_waiters();
    }
    // Only the listener actor opens epochs. No reader can retain a lock or guard.
    fn open(&self, epoch: Uuid) {
        self.epoch_version.fetch_add(1, Ordering::AcqRel);
        let (hi, lo) = epoch.as_u64_pair();
        self.epoch_hi.store(hi, Ordering::Release);
        self.epoch_lo.store(lo, Ordering::Release);
        self.captured_sequence.store(0, Ordering::Release);
        self.reason.store(0, Ordering::Release);
        self.closed.store(false, Ordering::Release);
        self.epoch_version.fetch_add(1, Ordering::Release);
        self.changed();
    }
    fn gap(&self, reason: SafeFailure) {
        self.reason.store(reason as u8, Ordering::Release);
        self.gap.fetch_add(1, Ordering::Release);
        self.changed();
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.changed();
    }
}
#[derive(Clone)]
pub struct CaptureMonitor {
    state: Arc<State>,
    observed: u64,
}
impl CaptureMonitor {
    pub fn state(&self) -> CaptureState {
        self.state.snapshot()
    }
    pub async fn changed(&mut self) -> CaptureState {
        loop {
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let revision = self.state.revision.load(Ordering::Acquire);
            if revision != self.observed {
                self.observed = revision;
                return self.state.snapshot();
            }
            notified.await;
        }
    }
}
pub struct CaptureReceiver {
    receiver: mpsc::Receiver<Frame>,
    state: Arc<State>,
}
impl CaptureReceiver {
    pub async fn recv(&mut self) -> Option<Frame> {
        self.receiver.recv().await
    }
    pub fn state(&self) -> CaptureState {
        self.state.snapshot()
    }
    pub fn subscribe_state(&self) -> CaptureMonitor {
        CaptureMonitor {
            state: self.state.clone(),
            observed: self.state.revision.load(Ordering::Acquire),
        }
    }
}
struct Capture {
    sender: mpsc::Sender<Frame>,
    budget: Arc<Semaphore>,
    state: Arc<State>,
}
impl Capture {
    fn gap(&self, reason: SafeFailure) {
        self.state.gap(reason);
    }
    fn observe(&self, epoch: Uuid, sequence: u64, direction: Direction, message: &Message) {
        // Publish before queue admission so a consumer cannot approve against
        // a decoded view while an already observed frame is still pending.
        self.state
            .captured_sequence
            .fetch_max(sequence, Ordering::AcqRel);
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(_) => {
                self.gap(SafeFailure::BinaryMessage);
                return;
            }
            _ => return,
        };
        let Ok(size) = u32::try_from(text.len()) else {
            self.gap(SafeFailure::CaptureLimit);
            return;
        };
        let Ok(permit) = self.budget.clone().try_acquire_many_owned(size) else {
            self.gap(SafeFailure::CaptureLimit);
            return;
        };
        let bytes: Bytes = text.clone().into();
        if let Err(error) = self.sender.try_send(Frame {
            connection_epoch: epoch,
            sequence,
            direction,
            bytes,
            _permit: permit,
        }) {
            self.gap(if matches!(error, mpsc::error::TrySendError::Closed(_)) {
                SafeFailure::CaptureDropped
            } else {
                SafeFailure::CaptureLimit
            });
        }
    }
}
/// Production pin established by the independent control connection. No RPC data.
#[derive(Clone, Copy)]
pub(crate) struct UpstreamIdentity {
    pub device: u64,
    pub inode: u64,
    pub process_id: i32,
    pub process_group: i32,
}
impl UpstreamIdentity {
    fn path_matches(self, path: &Path) -> Result<(), SafeFailure> {
        private_parent(path)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|_| SafeFailure::UnsafePath)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(SafeFailure::UnsafePath);
        }
        Ok(())
    }
    fn peer_matches(self, stream: &UnixStream) -> Result<(), SafeFailure> {
        let peer = stream.peer_cred().map_err(|_| SafeFailure::UnsafePath)?;
        if self.process_id <= 0
            || self.process_group <= 0
            || peer.pid() != Some(self.process_id)
            || peer.uid() != unsafe { libc::geteuid() }
            || unsafe { libc::getpgid(self.process_id) } != self.process_group
        {
            return Err(SafeFailure::UnsafePath);
        }
        Ok(())
    }
}
pub struct Relay {
    path: PathBuf,
    dev: u64,
    inode: u64,
    task: JoinHandle<()>,
    state: Arc<State>,
}
impl Relay {
    pub async fn bind(
        frontend: PathBuf,
        upstream: PathBuf,
        limits: RelayLimits,
    ) -> Result<(Self, CaptureReceiver), SafeFailure> {
        Self::bind_inner(frontend, upstream, limits, None).await
    }
    pub(crate) async fn bind_verified(
        frontend: PathBuf,
        upstream: PathBuf,
        limits: RelayLimits,
        identity: UpstreamIdentity,
    ) -> Result<(Self, CaptureReceiver), SafeFailure> {
        identity.path_matches(&upstream)?;
        Self::bind_inner(frontend, upstream, limits, Some(identity)).await
    }
    async fn bind_inner(
        frontend: PathBuf,
        upstream: PathBuf,
        limits: RelayLimits,
        identity: Option<UpstreamIdentity>,
    ) -> Result<(Self, CaptureReceiver), SafeFailure> {
        if limits.capture_bytes == 0
            || limits.capture_bytes > u32::MAX as usize
            || limits.capture_frames == 0
            || limits.capture_frames > 65536
        {
            return Err(SafeFailure::InvalidLimits);
        }
        private_parent(&frontend)?;
        private_parent(&upstream)?;
        if std::fs::symlink_metadata(&frontend).is_ok() {
            return Err(SafeFailure::SocketExists);
        }
        let target = std::fs::symlink_metadata(&upstream).map_err(|_| SafeFailure::UnsafePath)?;
        if !target.file_type().is_socket() || target.uid() != unsafe { libc::geteuid() } {
            return Err(SafeFailure::UnsafePath);
        }
        let listener = UnixListener::bind(&frontend).map_err(|_| SafeFailure::Bind)?;
        let metadata = std::fs::symlink_metadata(&frontend).map_err(|_| SafeFailure::Bind)?;
        let mut guard = SocketGuard {
            path: frontend.clone(),
            dev: metadata.dev(),
            inode: metadata.ino(),
        };
        std::fs::set_permissions(&frontend, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| SafeFailure::Bind)?;
        let (sender, receiver) = mpsc::channel(limits.capture_frames);
        let state = Arc::new(State::new());
        let capture = Arc::new(Capture {
            sender,
            budget: Arc::new(Semaphore::new(limits.capture_bytes)),
            state: state.clone(),
        });
        let task = tokio::spawn(async move {
            let mut active = FuturesUnordered::new();
            loop {
                tokio::select! {
                    result=listener.accept()=>{
                        let Ok((stream,_))=result else {capture.gap(SafeFailure::Transport);break;};
                        if !active.is_empty(){drop(stream);continue;}
                        let epoch=Uuid::new_v4();capture.state.open(epoch);
                        active.push(connection(stream,upstream.clone(),capture.clone(),epoch,identity));
                    }
                    _=active.next(),if !active.is_empty()=>{}
                }
            }
            // Dropping active futures closes both sockets; no detached pump tasks.
        });
        guard.path = PathBuf::new();
        Ok((
            Self {
                path: frontend,
                dev: metadata.dev(),
                inode: metadata.ino(),
                task,
                state: state.clone(),
            },
            CaptureReceiver { receiver, state },
        ))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
        self.state.close();
        self.task.abort();
        remove_owned(&self.path, self.dev, self.inode);
    }
}
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        remove_owned(&self.path, self.dev, self.inode);
    }
}
fn remove_owned(path: &Path, dev: u64, inode: u64) {
    if let Ok(m) = std::fs::symlink_metadata(path)
        && m.file_type().is_socket()
        && m.dev() == dev
        && m.ino() == inode
    {
        let _ = std::fs::remove_file(path);
    }
}
fn private_parent(path: &Path) -> Result<(), SafeFailure> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(SafeFailure::UnsafePath);
    }
    let parent = path.parent().ok_or(SafeFailure::UnsafePath)?;
    let m = std::fs::symlink_metadata(parent).map_err(|_| SafeFailure::UnsafePath)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o700
        || parent.canonicalize().map_err(|_| SafeFailure::UnsafePath)? != parent
    {
        return Err(SafeFailure::UnsafePath);
    }
    Ok(())
}
struct ConnectionGuard {
    capture: Arc<Capture>,
    epoch: Uuid,
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if self.capture.state.snapshot().connection_epoch == Some(self.epoch) {
            self.capture.state.close();
        }
    }
}
fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(WS_LIMIT))
        .max_message_size(Some(WS_LIMIT))
        .max_write_buffer_size(WS_LIMIT + 128 * 1024)
}
type Socket = WebSocketStream<HeaderLimited>;
async fn connection(
    front: UnixStream,
    upstream: PathBuf,
    capture: Arc<Capture>,
    epoch: Uuid,
    identity: Option<UpstreamIdentity>,
) {
    let _guard = ConnectionGuard {
        capture: capture.clone(),
        epoch,
    };
    let handshake = tokio::time::timeout(HANDSHAKE, async {
        // Recheck the private upstream leaf at connection time; never follow a symlink.
        private_parent(&upstream)?;
        let m = std::fs::symlink_metadata(&upstream).map_err(|_| SafeFailure::UnsafePath)?;
        if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } {
            return Err(SafeFailure::UnsafePath);
        }
        if let Some(identity) = identity {
            identity.path_matches(&upstream)?;
        }
        let back = UnixStream::connect(&upstream)
            .await
            .map_err(|_| SafeFailure::Transport)?;
        if let Some(identity) = identity {
            identity.peer_matches(&back)?;
            identity.path_matches(&upstream)?;
        }
        let (mut back, _) = client_async_with_config(
            "ws://localhost/rpc",
            HeaderLimited::new(back),
            Some(websocket_config()),
        )
        .await
        .map_err(|_| SafeFailure::Handshake)?;
        let mut front =
            accept_async_with_config(HeaderLimited::new(front), Some(websocket_config()))
                .await
                .map_err(|_| SafeFailure::Handshake)?;
        back.get_mut().remaining = None;
        front.get_mut().remaining = None;
        Ok::<_, SafeFailure>((front, back))
    })
    .await;
    let (front, back) = match handshake {
        Ok(Ok(pair)) => pair,
        Ok(Err(error)) => {
            capture.gap(error);
            return;
        }
        Err(_) => {
            capture.gap(SafeFailure::Handshake);
            return;
        }
    };
    let (front_sink, front_stream) = front.split();
    let (back_sink, back_stream) = back.split();
    let front_sink = Arc::new(Mutex::new((front_sink, false)));
    let back_sink = Arc::new(Mutex::new((back_sink, false)));
    let finished = Finished {
        notify: Notify::new(),
        failed: AtomicBool::new(false),
    };
    let sequence = AtomicU64::new(0);
    let both = async {
        tokio::join!(
            pump(
                front_stream,
                front_sink.clone(),
                back_sink.clone(),
                &capture,
                epoch,
                &sequence,
                Direction::ToServer,
                &finished
            ),
            pump(
                back_stream,
                back_sink,
                front_sink,
                &capture,
                epoch,
                &sequence,
                Direction::ToTui,
                &finished
            ),
        )
    };
    tokio::pin!(both);
    let result = tokio::select! {
        result=&mut both=>Some(result),
        _=finished.notify.notified()=>if finished.failed.load(Ordering::Acquire){None}else{tokio::time::timeout(Duration::from_secs(2),&mut both).await.ok()},
    };
    if !matches!(result, Some((Ok(()), Ok(())))) {
        capture.gap(SafeFailure::Transport);
    }
}
struct Finished {
    notify: Notify,
    failed: AtomicBool,
}
type SharedSink = Arc<Mutex<(SplitSink<Socket, Message>, bool)>>;
fn sink_result(result: Result<(), tokio_tungstenite::tungstenite::Error>) -> Result<(), ()> {
    match result {
        Ok(())
        | Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed)
        | Err(tokio_tungstenite::tungstenite::Error::AlreadyClosed) => Ok(()),
        Err(_) => Err(()),
    }
}
#[allow(clippy::too_many_arguments)]
async fn pump(
    mut source: SplitStream<Socket>,
    own_sink: SharedSink,
    target: SharedSink,
    capture: &Capture,
    epoch: Uuid,
    sequence: &AtomicU64,
    direction: Direction,
    finished: &Finished,
) -> Result<(), ()> {
    let result = async {
        let mut flushes = FuturesUnordered::new();
        loop {
            let next = tokio::select! {
                message=source.next()=>message,
                flushed=flushes.next(),if !flushes.is_empty()=>{flushed.unwrap()?;continue;}
            };
            let Some(message) = next else {
                return Ok(());
            };
            let message = match message {
                Ok(message) => message,
                Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed) => return Ok(()),
                Err(_) => return Err(()),
            };
            match message {
                Message::Ping(_) => {
                    // Flush auto-Pong concurrently with reads: a backpressured
                    // source sink must not prevent reading its following frame.
                    if flushes.is_empty() {
                        flushes.push(flush_heartbeat(own_sink.clone()));
                    }
                }
                Message::Pong(_) => {}
                Message::Close(frame) => {
                    // Revoke qualification before awaiting the bounded close handshake.
                    capture.state.close();
                    // Signal before touching sinks: close processing itself is bounded.
                    finished.notify.notify_one();
                    flushes.clear();
                    {
                        let mut sink = own_sink.lock().await;
                        sink.1 = true;
                        sink_result(sink.0.flush().await)?;
                    }
                    {
                        let mut sink = target.lock().await;
                        if !sink.1 {
                            sink.1 = true;
                            sink_result(sink.0.send(Message::Close(frame)).await)?;
                        }
                    }
                    return Ok(());
                }
                message => {
                    if message.is_text() || message.is_binary() {
                        let seq = sequence.fetch_add(1, Ordering::SeqCst) + 1;
                        capture.observe(epoch, seq, direction, &message);
                    }
                    let sending = async {
                        let mut sink = target.lock().await;
                        if sink.1 {
                            return Ok(());
                        }
                        sink.0.send(message).await.map_err(|_| ())
                    };
                    tokio::pin!(sending);
                    loop {
                        tokio::select! {
                            result=&mut sending=>{result?;break;}
                            flushed=flushes.next(),if !flushes.is_empty()=>{flushed.unwrap()?;}
                        }
                    }
                }
            }
        }
    }
    .await;
    // EOF, ConnectionClosed and errors also revoke before notifying the peer driver.
    capture.state.close();
    if result.is_err() {
        finished.failed.store(true, Ordering::Release);
    }
    finished.notify.notify_one();
    result
}
async fn flush_heartbeat(sink: SharedSink) -> Result<(), ()> {
    sink_result(sink.lock().await.0.flush().await)
}
// Tungstenite caps header count; additionally bound handshake bytes. The limit is
// disabled after negotiation. Native UDS clients wait for the HTTP upgrade reply.
struct HeaderLimited {
    stream: UnixStream,
    remaining: Option<usize>,
}
impl HeaderLimited {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            remaining: Some(HEADER_LIMIT),
        }
    }
}
impl AsyncRead for HeaderLimited {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(remaining) = self.remaining else {
            return Pin::new(&mut self.stream).poll_read(cx, buffer);
        };
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if remaining == 0 {
            return Poll::Ready(Err(io::Error::other("native relay handshake limit")));
        }
        let mut bytes = [0u8; 8192];
        let mut limited = ReadBuf::new(&mut bytes[..remaining.min(buffer.remaining()).min(8192)]);
        match Pin::new(&mut self.stream).poll_read(cx, &mut limited) {
            Poll::Ready(Ok(())) => {
                self.remaining = Some(remaining - limited.filled().len());
                buffer.put_slice(limited.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl AsyncWrite for HeaderLimited {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::{accept_async, client_async};

    async fn fixture(
        limits: RelayLimits,
    ) -> (tempfile::TempDir, Relay, CaptureReceiver, JoinHandle<()>) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let parent = root.path().canonicalize().unwrap();
        let upstream = parent.join("upstream.sock");
        let listener = UnixListener::bind(&upstream).unwrap();
        let task = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result=listener.accept()=>{let Ok((stream,_))=result else {break;};clients.spawn(async move{let Ok(mut ws)=accept_async(stream).await else{return;};while let Some(Ok(message))=ws.next().await {let closed=message.is_close();if ws.send(message).await.is_err()||closed{break;}}});}
                    _=clients.join_next(),if !clients.is_empty()=>{}
                }
            }
        });
        let (relay, receiver) = Relay::bind(parent.join("front.sock"), upstream, limits)
            .await
            .unwrap();
        (root, relay, receiver, task)
    }
    async fn client(path: &Path) -> WebSocketStream<UnixStream> {
        client_async(
            "ws://localhost/rpc",
            UnixStream::connect(path).await.unwrap(),
        )
        .await
        .unwrap()
        .0
    }
    async fn exchange(ws: &mut WebSocketStream<UnixStream>, message: Message) -> Message {
        ws.send(message).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }
    #[tokio::test]
    async fn verified_relay_rejects_wrong_peer_group_pid_and_replaced_socket() {
        for violation in 0..3 {
            let root = tempfile::tempdir().unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let parent = root.path().canonicalize().unwrap();
            let upstream = parent.join("native.sock");
            let _listener = UnixListener::bind(&upstream).unwrap();
            std::fs::set_permissions(&upstream, std::fs::Permissions::from_mode(0o600)).unwrap();
            let metadata = std::fs::symlink_metadata(&upstream).unwrap();
            let identity = UpstreamIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
                process_id: std::process::id() as i32 + i32::from(violation == 1),
                process_group: unsafe { libc::getpgrp() } + i32::from(violation == 0),
            };
            let (relay, capture) = Relay::bind_verified(
                parent.join("front.sock"),
                upstream.clone(),
                RelayLimits::default(),
                identity,
            )
            .await
            .unwrap();
            let _replacement = if violation == 2 {
                std::fs::remove_file(&upstream).unwrap();
                let listener = UnixListener::bind(&upstream).unwrap();
                std::fs::set_permissions(&upstream, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                Some(listener)
            } else {
                None
            };
            let stream = UnixStream::connect(relay.path()).await.unwrap();
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    client_async("ws://localhost/rpc", stream)
                )
                .await
                .unwrap()
                .is_err()
            );
            assert!(capture.state().closed);
            assert!(capture.state().gap_generation > 0);
            drop(relay);
            assert!(
                upstream.exists(),
                "relay does not own or remove upstream sockets"
            );
        }
    }
    #[tokio::test]
    async fn preserves_original_text_and_forwards_unknown_binary_with_gap() {
        let (_dir, relay, mut capture, task) = fixture(RelayLimits::default()).await;
        let mut ws = client(relay.path()).await;
        let original = "{ \"method\":\"unknown/test\", \"params\": {\"x\": 1} }\n";
        assert_eq!(
            exchange(&mut ws, Message::Text(original.into()))
                .await
                .into_text()
                .unwrap(),
            original
        );
        let first = capture.recv().await.unwrap();
        let second = capture.recv().await.unwrap();
        assert_eq!(first.bytes.as_ref(), original.as_bytes());
        assert_eq!(second.bytes, first.bytes);
        assert_eq!(
            (first.direction, second.direction),
            (Direction::ToServer, Direction::ToTui)
        );
        assert_eq!(first.sequence + 1, second.sequence);
        assert_eq!(first.connection_epoch, second.connection_epoch);
        let binary = Bytes::from_static(b"\x00\xff\x01");
        assert_eq!(
            exchange(&mut ws, Message::Binary(binary.clone())).await,
            Message::Binary(binary)
        );
        assert!(capture.state().gap_generation >= 1);
        assert_eq!(capture.state().reason, Some(SafeFailure::BinaryMessage));
        drop(relay);
        task.abort();
    }
    #[tokio::test]
    async fn full_slow_or_dropped_capture_never_blocks_forwarding() {
        let (_dir, relay, mut capture, task) = fixture(RelayLimits {
            capture_bytes: 8,
            capture_frames: 1,
        })
        .await;
        let mut ws = client(relay.path()).await;
        for _ in 0..32 {
            assert_eq!(
                exchange(&mut ws, Message::Text("test".into()))
                    .await
                    .into_text()
                    .unwrap(),
                "test"
            );
        }
        assert!(capture.state().gap_generation > 0);
        let retained = capture.recv().await.unwrap();
        drop(retained);
        let state = capture.subscribe_state();
        drop(capture);
        assert_eq!(
            exchange(&mut ws, Message::Text("still forwards".into()))
                .await
                .into_text()
                .unwrap(),
            "still forwards"
        );
        assert!(state.state().gap_generation > 0);
        drop(relay);
        task.abort();
    }
    #[tokio::test]
    async fn frame_drop_releases_byte_budget() {
        let (sender, mut receiver) = mpsc::channel(4);
        let state = Arc::new(State::new());
        let capture = Capture {
            sender,
            budget: Arc::new(Semaphore::new(4)),
            state,
        };
        let message = Message::Text("1234".into());
        capture.observe(Uuid::new_v4(), 1, Direction::ToTui, &message);
        assert_eq!(capture.budget.available_permits(), 0);
        let frame = receiver.recv().await.unwrap();
        assert_eq!(capture.budget.available_permits(), 0);
        drop(frame);
        assert_eq!(capture.budget.available_permits(), 4);
    }
    #[tokio::test]
    async fn rejects_second_tui_and_accepts_new_epoch_after_close() {
        let (_dir, relay, mut capture, task) = fixture(RelayLimits::default()).await;
        let mut first = client(relay.path()).await;
        exchange(&mut first, Message::Text("one".into())).await;
        let epoch = capture.recv().await.unwrap().connection_epoch;
        drop(capture.recv().await);
        let second = UnixStream::connect(relay.path()).await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                client_async("ws://localhost/rpc", second)
            )
            .await
            .unwrap()
            .is_err()
        );
        exchange(&mut first, Message::Text("still one".into())).await;
        drop(capture.recv().await);
        drop(capture.recv().await);
        drop(first);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !capture.state().closed {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut new = client(relay.path()).await;
        exchange(&mut new, Message::Text("new".into())).await;
        assert_ne!(capture.recv().await.unwrap().connection_epoch, epoch);
        drop(relay);
        task.abort();
    }
    #[tokio::test]
    async fn drop_removes_owned_socket_and_closes_forwarding() {
        let (_dir, relay, capture, task) = fixture(RelayLimits::default()).await;
        let mut ws = client(relay.path()).await;
        exchange(&mut ws, Message::Text("ready".into())).await;
        let path = relay.path().to_owned();
        drop(relay);
        assert!(!path.exists());
        assert!(capture.state().closed);
        let result = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap();
        assert!(result.is_none() || result.unwrap().is_err());
        task.abort();
    }
    #[tokio::test]
    async fn rejects_unsafe_parent_and_existing_leaf_without_removing_it() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().canonicalize().unwrap();
        let upstream = parent.join("upstream.sock");
        let _listener = UnixListener::bind(&upstream).unwrap();
        let front = parent.join("front.sock");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            Relay::bind(front.clone(), upstream.clone(), RelayLimits::default()).await,
            Err(SafeFailure::UnsafePath)
        ));
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&front, b"owned fixture").unwrap();
        assert!(matches!(
            Relay::bind(front.clone(), upstream.clone(), RelayLimits::default()).await,
            Err(SafeFailure::SocketExists)
        ));
        assert_eq!(std::fs::read(&front).unwrap(), b"owned fixture");
        std::fs::remove_file(&front).unwrap();
        std::os::unix::fs::symlink(&upstream, &front).unwrap();
        assert!(matches!(
            Relay::bind(front, upstream, RelayLimits::default()).await,
            Err(SafeFailure::SocketExists)
        ));
    }
    fn restrict_test_socket_buffers(stream: &UnixStream) {
        use std::os::fd::AsRawFd;
        let bytes: libc::c_int = 4096;
        for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            // Test-only: force real backpressure independently of OS defaults.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        stream.as_raw_fd(),
                        libc::SOL_SOCKET,
                        option,
                        (&bytes as *const libc::c_int).cast(),
                        std::mem::size_of_val(&bytes) as libc::socklen_t,
                    )
                },
                0
            );
        }
    }
    async fn raw_pair() -> (
        tempfile::TempDir,
        Relay,
        CaptureReceiver,
        WebSocketStream<UnixStream>,
        WebSocketStream<UnixStream>,
    ) {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let parent = root.path().canonicalize().unwrap();
        let upstream = parent.join("raw.sock");
        let listener = UnixListener::bind(&upstream).unwrap();
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            restrict_test_socket_buffers(&stream);
            accept_async(stream).await.unwrap()
        });
        let (relay, capture) =
            Relay::bind(parent.join("front.sock"), upstream, RelayLimits::default())
                .await
                .unwrap();
        let stream = UnixStream::connect(relay.path()).await.unwrap();
        restrict_test_socket_buffers(&stream);
        let front = client_async("ws://localhost/rpc", stream).await.unwrap().0;
        let back = accept.await.unwrap();
        (root, relay, capture, front, back)
    }
    #[tokio::test]
    async fn held_state_snapshot_and_pending_monitor_cannot_block_overflow() {
        let (_root, relay, capture, task) = fixture(RelayLimits {
            capture_bytes: 1,
            capture_frames: 1,
        })
        .await;
        let mut monitor = capture.subscribe_state();
        let snapshot = monitor.state();
        let waiting = monitor.changed();
        tokio::pin!(waiting);
        let mut ws = client(relay.path()).await;
        for _ in 0..8 {
            assert_eq!(
                exchange(&mut ws, Message::Text("overflow".into()))
                    .await
                    .into_text()
                    .unwrap(),
                "overflow"
            );
        }
        assert!(snapshot.closed);
        assert!(capture.state().gap_generation > 0);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .is_ok()
        );
        drop(relay);
        task.abort();
    }
    #[tokio::test]
    async fn heartbeats_terminate_per_leg_without_duplicate_pongs() {
        let (_root, relay, capture, mut front, mut back) = raw_pair().await;
        front
            .send(Message::Ping(Bytes::from_static(b"front")))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), front.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Pong(Bytes::from_static(b"front"))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(40), back.next())
                .await
                .is_err()
        );
        front
            .send(Message::Text("after front ping".into()))
            .await
            .unwrap();
        assert_eq!(
            back.next().await.unwrap().unwrap().into_text().unwrap(),
            "after front ping"
        );
        back.send(Message::Ping(Bytes::from_static(b"back")))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), back.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Pong(Bytes::from_static(b"back"))
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(40), front.next())
                .await
                .is_err()
        );
        back.send(Message::Text("after back ping".into()))
            .await
            .unwrap();
        assert_eq!(
            front.next().await.unwrap().unwrap().into_text().unwrap(),
            "after back ping"
        );
        assert_eq!(capture.state().gap_generation, 0);
        drop(relay);
    }
    #[tokio::test]
    async fn either_leg_close_completes_both_handshakes_without_reset() {
        for from_front in [true, false] {
            let (_root, relay, capture, mut front, mut back) = raw_pair().await;
            let (source, target) = if from_front {
                (&mut front, &mut back)
            } else {
                (&mut back, &mut front)
            };
            source.close(None).await.unwrap();
            let ack = tokio::time::timeout(Duration::from_secs(1), source.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(ack.is_close());
            let close = tokio::time::timeout(Duration::from_secs(1), target.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(close.is_close());
            target.flush().await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while !capture.state().closed {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(capture.state().gap_generation, 0);
            drop(relay);
        }
    }
    #[tokio::test]
    async fn tui_close_revokes_capture_before_late_upstream_final() {
        let (_root, relay, mut capture, mut front, mut back) = raw_pair().await;
        front.close(None).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), front.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_close()
        );
        // The upstream has not read or acknowledged Close, so cleanup is unfinished.
        assert!(capture.state().closed);
        let final_message = r#"{"method":"turn/completed","params":{"synthetic":true}}"#;
        back.send(Message::Text(final_message.into()))
            .await
            .unwrap();
        let late = tokio::time::timeout(Duration::from_secs(1), capture.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(late.direction, Direction::ToTui);
        assert_eq!(late.bytes.as_ref(), final_message.as_bytes());
        assert!(
            capture.state().closed,
            "late evidence cannot restore qualification"
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), back.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .is_close()
        );
        back.flush().await.unwrap();
        drop(relay);
    }
    #[test]
    fn snapshot_retries_when_epoch_opens_between_identity_and_state_reads() {
        let state = State::new();
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        state.open(old);
        state.close();
        let mut replace = Some(new);
        // Deterministically reproduce the concurrent writer at the former window.
        let snapshot = state.snapshot_with(|| {
            if let Some(epoch) = replace.take() {
                state.open(epoch);
            }
        });
        assert_eq!(snapshot.connection_epoch, Some(new));
        assert!(!snapshot.closed);
    }
    #[tokio::test]
    async fn both_endpoints_send_large_frames_before_either_reads() {
        let (_root, relay, capture, mut front, mut back) = raw_pair().await;
        let from_front = "A".repeat(2 * 1024 * 1024);
        let from_back = "B".repeat(2 * 1024 * 1024);
        tokio::time::timeout(Duration::from_secs(5), async {
            let (send_front, send_back) = tokio::join!(
                async {
                    front
                        .send(Message::Text(from_front.clone().into()))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    front.next().await.unwrap().unwrap()
                },
                async {
                    back.send(Message::Text(from_back.clone().into()))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    back.next().await.unwrap().unwrap()
                },
            );
            assert_eq!(send_front.into_text().unwrap(), from_back);
            assert_eq!(send_back.into_text().unwrap(), from_front);
        })
        .await
        .unwrap();
        assert_eq!(capture.state().gap_generation, 0);
        drop(relay);
    }
    #[tokio::test]
    async fn blocked_forward_write_does_not_prevent_reverse_read() {
        let (_root, relay, capture, mut front, mut back) = raw_pair().await;
        let payload = "X".repeat(2 * 1024 * 1024);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    for _ in 0..2 {
                        front
                            .send(Message::Text(payload.clone().into()))
                            .await
                            .unwrap();
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    assert_eq!(
                        front.next().await.unwrap().unwrap().into_text().unwrap(),
                        payload
                    );
                },
                async {
                    // Let the relay enter its forward send before supplying the reverse frame.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    back.send(Message::Text(payload.clone().into()))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    for _ in 0..2 {
                        assert_eq!(
                            back.next().await.unwrap().unwrap().into_text().unwrap(),
                            payload
                        );
                    }
                }
            );
        })
        .await
        .unwrap();
        assert_eq!(capture.state().gap_generation, 0);
        drop(relay);
    }
    #[tokio::test]
    async fn simultaneous_ping_then_large_frame_does_not_block_reads_on_flush() {
        let (_root, relay, _capture, mut front, mut back) = raw_pair().await;
        let payload = "X".repeat(2 * 1024 * 1024);
        async fn endpoint(ws: &mut WebSocketStream<UnixStream>, payload: &str) {
            ws.send(Message::Ping(Bytes::from_static(b"alive")))
                .await
                .unwrap();
            ws.send(Message::Text(payload.into())).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut pong = 0;
            let mut text = 0;
            for _ in 0..2 {
                match ws.next().await.unwrap().unwrap() {
                    Message::Pong(bytes) => {
                        assert_eq!(bytes, Bytes::from_static(b"alive"));
                        pong += 1;
                    }
                    Message::Text(bytes) => {
                        assert_eq!(bytes, payload);
                        text += 1;
                    }
                    _ => panic!("unexpected heartbeat relay message"),
                }
            }
            assert_eq!((pong, text), (1, 1));
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                endpoint(&mut front, &payload),
                endpoint(&mut back, &payload)
            );
        })
        .await
        .unwrap();
        drop(relay);
    }
}

#[cfg(test)]
mod watermark_tests {
    use super::*;
    #[tokio::test]
    async fn watermark_precedes_decode_and_survives_capture_rejection() {
        let state = Arc::new(State::new());
        let epoch = Uuid::new_v4();
        state.open(epoch);
        let (sender, receiver) = mpsc::channel(1);
        let capture = Capture {
            sender,
            budget: Arc::new(Semaphore::new(32)),
            state: state.clone(),
        };
        let mut receiver = CaptureReceiver {
            receiver,
            state: state.clone(),
        };
        let monitor = receiver.subscribe_state();
        let clone = monitor.clone();
        capture.observe(epoch, 1, Direction::ToServer, &Message::Text("one".into()));
        assert_eq!(monitor.state().captured_sequence, 1);
        assert_eq!(clone.state().captured_sequence, 1);
        capture.observe(epoch, 2, Direction::ToTui, &Message::Text("two".into()));
        assert_eq!(clone.state().captured_sequence, 2);
        assert!(clone.state().gap_generation > 0);
        assert_eq!(receiver.recv().await.unwrap().sequence, 1);
        state.open(Uuid::new_v4());
        assert_eq!(monitor.state().captured_sequence, 0);
    }
    #[test]
    fn sequence_cannot_regress_when_capture_producers_arrive_out_of_order() {
        let state = Arc::new(State::new());
        let epoch = Uuid::new_v4();
        state.open(epoch);
        let (sender, _receiver) = mpsc::channel(4);
        let capture = Capture {
            sender,
            budget: Arc::new(Semaphore::new(32)),
            state: state.clone(),
        };
        capture.observe(epoch, 2, Direction::ToTui, &Message::Text("two".into()));
        capture.observe(epoch, 1, Direction::ToServer, &Message::Text("one".into()));
        assert_eq!(state.snapshot().captured_sequence, 2);
    }
}
