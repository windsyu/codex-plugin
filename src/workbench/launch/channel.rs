//! Explicit native channel launch. No thread creation, resume, queue or subscription RPC.
use super::super::channel::native::{CaptureReceiver, Relay, UpstreamIdentity};
use crate::workbench::channel::interaction::{NativeResponse, RpcId};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fmt,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::UnixStream,
    sync::{Notify, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(20);
const MESSAGE_LIMIT: usize = 1024 * 1024;
const TEXT_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ControlFailure {
    Stopped = 1,
    Busy,
    InvalidInput,
    Transport,
    Timeout,
    Protocol,
    Startup,
    StartupProcess,
    StartupSocket,
    StartupRelay,
}
impl fmt::Display for ControlFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native channel {self:?}")
    }
}
impl std::error::Error for ControlFailure {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submission {
    Accepted {
        turn_id: Uuid,
    },
    Rejected {
        code: i64,
    },
    /// No attempt to write this request was made.
    NotSent {
        reason: ControlFailure,
    },
    /// A write was attempted. Native execution may have begun; never replay automatically.
    Unknown {
        reason: ControlFailure,
    },
}
/// Native answers have no application-level acknowledgment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponseWrite {
    NotSent { reason: ControlFailure },
    WrittenUnconfirmed,
    Unknown { reason: ControlFailure },
}
struct State {
    failure: AtomicU8,
    notify: Notify,
}
impl State {
    fn new() -> Self {
        Self {
            failure: AtomicU8::new(0),
            notify: Notify::new(),
        }
    }
    fn failure(&self) -> Option<ControlFailure> {
        match self.failure.load(Ordering::Acquire) {
            0 => None,
            1 => Some(ControlFailure::Stopped),
            4 => Some(ControlFailure::Transport),
            5 => Some(ControlFailure::Timeout),
            _ => Some(ControlFailure::Protocol),
        }
    }
    fn stop(&self, failure: ControlFailure) {
        let _ =
            self.failure
                .compare_exchange(0, failure as u8, Ordering::AcqRel, Ordering::Acquire);
        self.notify.notify_waiters();
    }
    async fn stopped(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.failure().is_some() {
                return;
            }
            notified.await;
        }
    }
}
enum Request {
    Start {
        thread: Uuid,
        text: String,
        client: Uuid,
        reply: oneshot::Sender<Submission>,
    },
    Answer {
        id: RpcId,
        response: NativeResponse,
        validate: Box<dyn FnOnce() -> bool + Send>,
        reply: oneshot::Sender<ResponseWrite>,
    },
}
impl Request {
    fn is_closed(&self) -> bool {
        match self {
            Self::Start { reply, .. } => reply.is_closed(),
            Self::Answer { reply, .. } => reply.is_closed(),
        }
    }
    fn not_sent(self, reason: ControlFailure) {
        match self {
            Self::Start { reply, .. } => {
                let _ = reply.send(Submission::NotSent { reason });
            }
            Self::Answer { reply, .. } => {
                let _ = reply.send(ResponseWrite::NotSent { reason });
            }
        }
    }
}
/// Cloneable submission endpoint, deliberately without Debug or generic RPC access.
#[derive(Clone)]
pub struct ControlHandle {
    sender: mpsc::Sender<Request>,
    state: Arc<State>,
}
impl ControlHandle {
    #[cfg(test)]
    pub(crate) fn test_socket(socket: WebSocketStream<UnixStream>) -> (Self, JoinHandle<()>) {
        control(socket, Duration::from_secs(2))
    }
    pub fn failure(&self) -> Option<ControlFailure> {
        self.state.failure()
    }
    pub fn is_closed(&self) -> bool {
        self.failure().is_some()
    }
    pub async fn closed(&self) -> ControlFailure {
        self.state.stopped().await;
        self.failure().unwrap_or(ControlFailure::Stopped)
    }
    pub async fn turn_start(&self, thread: Uuid, text: String, client: Uuid) -> Submission {
        if thread.is_nil() || client.is_nil() || text.is_empty() || text.len() > TEXT_LIMIT {
            return Submission::NotSent {
                reason: ControlFailure::InvalidInput,
            };
        }
        if let Some(reason) = self.failure() {
            return Submission::NotSent { reason };
        }
        let (reply, receiver) = oneshot::channel();
        match self.sender.try_send(Request::Start {
            thread,
            text,
            client,
            reply,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return Submission::NotSent {
                    reason: ControlFailure::Busy,
                };
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Submission::NotSent {
                    reason: self.failure().unwrap_or(ControlFailure::Stopped),
                };
            }
        }
        // Cancellation after enqueue is deliberately not a retry signal. The caller's
        // durable submission intent must remain uncertain until native evidence settles it.
        receiver.await.unwrap_or(Submission::Unknown {
            reason: self.failure().unwrap_or(ControlFailure::Stopped),
        })
    }
    /// Write only a schema-checked native answer, with a fresh authorization gate.
    pub(crate) async fn respond(
        &self,
        id: RpcId,
        response: NativeResponse,
        validate: Box<dyn FnOnce() -> bool + Send>,
    ) -> ResponseWrite {
        if RpcId::parse(&id.to_value()).is_err()
            || response.envelope(&id).to_string().len() > MESSAGE_LIMIT
        {
            return ResponseWrite::NotSent {
                reason: ControlFailure::InvalidInput,
            };
        }
        if let Some(reason) = self.failure() {
            return ResponseWrite::NotSent { reason };
        }
        let (reply, receiver) = oneshot::channel();
        match self.sender.try_send(Request::Answer {
            id,
            response,
            validate,
            reply,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                return ResponseWrite::NotSent {
                    reason: ControlFailure::Busy,
                };
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return ResponseWrite::NotSent {
                    reason: self.failure().unwrap_or(ControlFailure::Stopped),
                };
            }
        }
        receiver.await.unwrap_or(ResponseWrite::Unknown {
            reason: self.failure().unwrap_or(ControlFailure::Stopped),
        })
    }
}
pub struct NativeAttachment {
    pub capture: CaptureReceiver,
    pub control: ControlHandle,
}
struct Process(Mutex<Child>, AtomicBool);
impl Process {
    fn new(child: Child) -> Self {
        Self(Mutex::new(child), AtomicBool::new(false))
    }
    fn reap_normal(&self, peer: i32) -> bool {
        let Ok(mut child) = self.0.lock() else {
            return false;
        };
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
            || unsafe { info.si_pid() } != child.id() as i32
            || info.si_code != libc::CLD_EXITED
            || unsafe { info.si_status() } != 0
        {
            return false;
        }
        if peer != child.id() as i32
            && (unsafe { libc::kill(peer, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
        {
            return false;
        }
        match child.wait() {
            Ok(status) => {
                self.1.store(true, Ordering::Release);
                status.success()
            }
            Err(error) => {
                // Another reaper must never turn a stale leader ID into a kill
                // target. This violates ownership, so do not certify success.
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.1.store(true, Ordering::Release);
                }
                false
            }
        }
    }
    fn healthy(&self) -> bool {
        self.0.lock().ok().is_some_and(|child| {
            // Inspect without reaping: retain the leader PID until group teardown,
            // so a later Drop cannot target a reused process group identifier.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            result == 0 && unsafe { info.si_pid() } == 0
        })
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.1.load(Ordering::Acquire) {
            return;
        }
        if let Ok(child) = self.0.get_mut() {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
            {
                return;
            }
            // A distribution entrypoint can be a Node wrapper. A new group was
            // created at spawn; terminate only that owned group, then reap our child.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
// The official Unix listener publishes an alias into a shared, private native
// socket directory. Only our exact alias is resolved, then identity is pinned.
#[derive(Clone, PartialEq, Eq)]
struct SocketIdentity {
    path: PathBuf,
    dev: u64,
    inode: u64,
    alias_dev: u64,
    alias_inode: u64,
}
fn private_directory(path: &Path) -> Result<(), ControlFailure> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| ControlFailure::StartupSocket)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
        || path
            .canonicalize()
            .map_err(|_| ControlFailure::StartupSocket)?
            != path
    {
        return Err(ControlFailure::StartupSocket);
    }
    Ok(())
}
fn valid_alias_metadata(metadata: &std::fs::Metadata, owner: u32) -> bool {
    metadata.uid() == owner
        && (metadata.file_type().is_symlink() || metadata.file_type().is_socket())
}
impl SocketIdentity {
    fn resolve(alias: &Path, parent: &Path) -> Result<Self, ControlFailure> {
        private_directory(parent)?;
        if alias != parent.join("app.sock") {
            return Err(ControlFailure::StartupSocket);
        }
        let advertised =
            std::fs::symlink_metadata(alias).map_err(|_| ControlFailure::StartupSocket)?;
        if !valid_alias_metadata(&advertised, unsafe { libc::geteuid() }) {
            return Err(ControlFailure::StartupSocket);
        }
        let path = alias
            .canonicalize()
            .map_err(|_| ControlFailure::StartupSocket)?;
        private_directory(path.parent().ok_or(ControlFailure::StartupSocket)?)?;
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| ControlFailure::StartupSocket)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(ControlFailure::StartupSocket);
        }
        Ok(Self {
            path,
            dev: metadata.dev(),
            inode: metadata.ino(),
            alias_dev: advertised.dev(),
            alias_inode: advertised.ino(),
        })
    }
    fn verify_connected(
        &self,
        stream: &UnixStream,
        group: u32,
        alias: &Path,
        parent: &Path,
    ) -> Result<(), ControlFailure> {
        let peer = stream
            .peer_cred()
            .map_err(|_| ControlFailure::StartupSocket)?;
        let pid = peer.pid().ok_or(ControlFailure::StartupSocket)?;
        if peer.uid() != unsafe { libc::geteuid() }
            || group == 0
            || unsafe { libc::getpgid(pid) } != group as i32
            || Self::resolve(alias, parent)? != *self
        {
            return Err(ControlFailure::StartupSocket);
        }
        Ok(())
    }
}
struct OwnedSocket(SocketIdentity);
impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if let Ok(metadata) = std::fs::symlink_metadata(&self.0.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.0.dev
            && metadata.ino() == self.0.inode
        {
            let _ = std::fs::remove_file(&self.0.path);
        }
    }
}
/// The runtime retains ownership even after the actor takes its observation and handle.
pub(super) struct Resources {
    relay: Option<Relay>,
    alias: PathBuf,
    peer: i32,
    control_task: JoinHandle<()>,
    control: ControlHandle,
    process: Process,
    _physical_socket: OwnedSocket,
    _directory: tempfile::TempDir,
}
pub(super) struct Owner(Option<Resources>);
impl std::ops::Deref for Owner {
    type Target = Resources;
    fn deref(&self) -> &Resources {
        self.0.as_ref().expect("live owner")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Verified native peer is gone and its owned wrapper exited successfully
    /// and was reaped without a forced fallback. This says nothing about tools
    /// that the native runtime detached or terminated asynchronously.
    NativeExited,
    /// Deadline, protocol failure or forced owned-group fallback prevented a
    /// normal native lifecycle result. Detached tool state may be unknown.
    ForcedUnknown,
}
#[derive(Clone)]
pub struct ShutdownHandle(Arc<AtomicU8>);
impl ShutdownHandle {
    #[cfg(test)]
    pub(crate) fn test_outcome(outcome: Option<ShutdownOutcome>) -> Self {
        Self(Arc::new(AtomicU8::new(match outcome {
            None => 0,
            Some(ShutdownOutcome::NativeExited) => 1,
            Some(ShutdownOutcome::ForcedUnknown) => 2,
        })))
    }
    pub fn outcome(&self) -> Option<ShutdownOutcome> {
        match self.0.load(Ordering::Acquire) {
            1 => Some(ShutdownOutcome::NativeExited),
            2 => Some(ShutdownOutcome::ForcedUnknown),
            _ => None,
        }
    }
    pub fn is_finished(&self) -> bool {
        self.outcome().is_some()
    }
}
impl Owner {
    pub(super) fn begin_shutdown(mut self) -> ShutdownHandle {
        start_shutdown(self.0.take().expect("live owner"))
    }
    pub(super) async fn start(
        binary: &Path,
        home: &Path,
        cwd: &Path,
        provider: &str,
        proxy_url: &str,
    ) -> Result<(Self, NativeAttachment), ControlFailure> {
        let directory = tempfile::Builder::new()
            .prefix("cxch-")
            .tempdir()
            .map_err(|_| ControlFailure::Startup)?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|_| ControlFailure::Startup)?;
        let parent = directory
            .path()
            .canonicalize()
            .map_err(|_| ControlFailure::Startup)?;
        let upstream = parent.join("app.sock");
        let child = Command::new(binary)
            .args([
                "-c",
                &format!(
                    "model_providers.{provider}.base_url={}",
                    toml::Value::String(proxy_url.to_owned())
                ),
            ])
            .args(["-c", "thread_unload_delay_secs=0"])
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", upstream.display()))
            .env("CODEX_HOME", home)
            .current_dir(cwd)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ControlFailure::Startup)?;
        let process = Process::new(child);
        let physical = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !process.healthy() {
                    return Err(ControlFailure::StartupProcess);
                }
                match std::fs::symlink_metadata(&upstream) {
                    Ok(_) => return SocketIdentity::resolve(&upstream, &parent),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(ControlFailure::Startup),
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| ControlFailure::Timeout)??;
        let stream =
            tokio::time::timeout(Duration::from_secs(10), UnixStream::connect(&physical.path))
                .await
                .map_err(|_| ControlFailure::Timeout)?
                .map_err(|_| ControlFailure::Transport)?;
        let group = process
            .0
            .lock()
            .map_err(|_| ControlFailure::StartupProcess)?
            .id();
        // Verify kernel peer and alias identity before any handshake or protocol write.
        physical.verify_connected(&stream, group, &upstream, &parent)?;
        let owned_socket = OwnedSocket(physical);
        let socket = tokio::time::timeout(Duration::from_secs(10), initialize(stream))
            .await
            .map_err(|_| ControlFailure::Timeout)??;
        owned_socket
            .0
            .verify_connected(socket.get_ref(), group, &upstream, &parent)?;
        let identity = UpstreamIdentity {
            device: owned_socket.0.dev,
            inode: owned_socket.0.inode,
            process_id: socket
                .get_ref()
                .peer_cred()
                .map_err(|_| ControlFailure::StartupSocket)?
                .pid()
                .ok_or(ControlFailure::StartupSocket)?,
            process_group: group as i32,
        };
        let (relay, capture) = Relay::bind_verified(
            parent.join("tui.sock"),
            owned_socket.0.path.clone(),
            Default::default(),
            identity,
        )
        .await
        .map_err(|_| ControlFailure::StartupRelay)?;
        let (control, control_task) = control(socket, DEADLINE);
        let attachment = NativeAttachment {
            capture,
            control: control.clone(),
        };
        Ok((
            Self(Some(Resources {
                relay: Some(relay),
                alias: upstream,
                peer: identity.process_id,
                control_task,
                control,
                process,
                _physical_socket: owned_socket,
                _directory: directory,
            })),
            attachment,
        ))
    }
    pub(super) fn path(&self) -> &Path {
        self.relay.as_ref().expect("live relay").path()
    }
    pub(super) fn healthy(&self) -> bool {
        self.process.healthy()
            && self
                .relay
                .as_ref()
                .is_some_and(|relay| !relay.is_finished())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(resources) = self.0.take() {
            // Explicit Run stop uses begin_shutdown and remains nonblocking.
            // This fallback covers unpublished startup errors/cancellation and
            // standalone Runtime destruction; the host must not outrun reaping.
            let shutdown = start_shutdown(resources);
            let deadline = std::time::Instant::now() + Duration::from_secs(18);
            while !shutdown.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if shutdown.outcome() != Some(ShutdownOutcome::NativeExited) {
                eprintln!(
                    "codex-view: native_shutdown_incomplete; detached tool cleanup is not confirmed"
                );
            }
        }
    }
}
fn start_shutdown(mut resources: Resources) -> ShutdownHandle {
    resources.control.state.stop(ControlFailure::Stopped);
    resources.control_task.abort();
    drop(resources.relay.take());
    let handle = ShutdownHandle(Arc::new(AtomicU8::new(0)));
    let state = handle.0.clone();
    // Resources stay in this slot if spawning fails, so failure still kills and
    // reaps only the owned group before publishing the conservative outcome.
    let owned = Arc::new(Mutex::new(Some(resources)));
    let worker = owned.clone();
    let finished = state.clone();
    let spawned = std::thread::Builder::new()
        .name("native-shutdown".into())
        .spawn(move || {
            let Some(resources) = worker.lock().ok().and_then(|mut slot| slot.take()) else {
                return;
            };
            let native_exited = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
                .is_some_and(|runtime| {
                    runtime.block_on(async {
                        matches!(
                            tokio::time::timeout(
                                Duration::from_secs(15),
                                shutdown_native(&resources)
                            )
                            .await,
                            Ok(Ok(()))
                        )
                    }) && resources.process.reap_normal(resources.peer)
                });
            // Drop owns the bounded fallback: never infer that a foreign process
            // group was cleaned merely because the app-server group was reaped.
            drop(resources);
            finished.store(if native_exited { 1 } else { 2 }, Ordering::Release);
        });
    if spawned.is_err() {
        if let Ok(mut slot) = owned.lock() {
            drop(slot.take());
        }
        state.store(2, Ordering::Release);
    }
    handle
}
async fn shutdown_rpc(
    socket: &mut Socket,
    id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, ControlFailure> {
    *id += 1;
    socket
        .send(Message::Text(
            json!({"id":*id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|_| ControlFailure::Transport)?;
    let value = response(socket, *id).await?;
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        _ => Err(ControlFailure::Protocol),
    }
}
async fn shutdown_native(resources: &Resources) -> Result<(), ControlFailure> {
    if !resources.process.healthy() {
        return Ok(());
    }
    let physical = &resources._physical_socket.0;
    let alias = &resources.alias;
    let parent = alias.parent().ok_or(ControlFailure::StartupSocket)?;
    let group = resources
        .process
        .0
        .lock()
        .map_err(|_| ControlFailure::StartupProcess)?
        .id();
    let raw = UnixStream::connect(&physical.path)
        .await
        .map_err(|_| ControlFailure::Transport)?;
    physical.verify_connected(&raw, group, alias, parent)?;
    if raw.peer_cred().ok().and_then(|cred| cred.pid()) != Some(resources.peer) {
        return Err(ControlFailure::StartupSocket);
    }
    let mut socket = initialize(raw).await?;
    physical.verify_connected(socket.get_ref(), group, alias, parent)?;
    let peer = socket
        .get_ref()
        .peer_cred()
        .map_err(|_| ControlFailure::StartupSocket)?
        .pid()
        .ok_or(ControlFailure::StartupSocket)?;
    // Signal only the verified native peer. Signalling its Node wrapper/group
    // can forward a second signal, which skips official thread cleanup.
    if unsafe { libc::kill(peer, libc::SIGTERM) } != 0 {
        return Err(ControlFailure::Transport);
    }
    let mut id = 0;
    let attempt = async {
        let result = shutdown_rpc(
            &mut socket,
            &mut id,
            "thread/loaded/list",
            json!({"limit":100}),
        )
        .await?;
        if result
            .get("nextCursor")
            .is_some_and(|cursor| !cursor.is_null())
        {
            return Err(ControlFailure::Protocol);
        }
        let threads = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or(ControlFailure::Protocol)?;
        for thread in threads {
            let thread = thread
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .filter(|value| !value.is_nil())
                .ok_or(ControlFailure::Protocol)?;
            let turns = shutdown_rpc(
                &mut socket,
                &mut id,
                "thread/turns/list",
                json!({"threadId":thread,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"}),
            )
            .await?;
            let turns = turns
                .get("data")
                .and_then(Value::as_array)
                .ok_or(ControlFailure::Protocol)?;
            for turn in turns {
                if turn.get("status").and_then(Value::as_str) == Some("inProgress") {
                    let turn = turn
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|value| Uuid::parse_str(value).ok())
                        .filter(|value| !value.is_nil())
                        .ok_or(ControlFailure::Protocol)?;
                    let _ = shutdown_rpc(
                        &mut socket,
                        &mut id,
                        "turn/interrupt",
                        json!({"threadId":thread,"turnId":turn}),
                    )
                    .await?;
                }
            }
            let _ = shutdown_rpc(
                &mut socket,
                &mut id,
                "thread/backgroundTerminals/clean",
                json!({"threadId":thread}),
            )
            .await?;
        }
        Ok::<_, ControlFailure>(())
    };
    // The native server may close the socket while completing its own cleanup.
    // RPC acceptance is never treated as tool termination or native process exit.
    match tokio::time::timeout(Duration::from_secs(3), attempt).await {
        Ok(Ok(())) | Ok(Err(ControlFailure::Stopped | ControlFailure::Transport)) => {}
        Ok(Err(reason)) => return Err(reason),
        Err(_) => return Err(ControlFailure::Timeout),
    }
    while resources.process.healthy() {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}
type Socket = WebSocketStream<UnixStream>;
async fn initialize(stream: UnixStream) -> Result<Socket, ControlFailure> {
    let (mut socket, _) = client_async_with_config(
        "ws://localhost/rpc",
        stream,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MESSAGE_LIMIT))
                .max_frame_size(Some(MESSAGE_LIMIT)),
        ),
    )
    .await
    .map_err(|_| ControlFailure::Transport)?;
    socket.send(Message::Text(json!({"id":0,"method":"initialize","params":{"clientInfo":{"name":"codex_local_gateway_channel","version":"1"},"capabilities":{"experimentalApi":true}}}).to_string().into())).await.map_err(|_| ControlFailure::Transport)?;
    let response = response(&mut socket, 0).await?;
    if !response.get("result").is_some_and(Value::is_object) || response.get("error").is_some() {
        return Err(ControlFailure::Protocol);
    }
    socket
        .send(Message::Text(
            json!({"method":"initialized"}).to_string().into(),
        ))
        .await
        .map_err(|_| ControlFailure::Transport)?;
    Ok(socket)
}
fn control(mut socket: Socket, deadline: Duration) -> (ControlHandle, JoinHandle<()>) {
    let (sender, mut receiver) = mpsc::channel::<Request>(1);
    let state = Arc::new(State::new());
    let handle = ControlHandle {
        sender,
        state: state.clone(),
    };
    let task = tokio::spawn(async move {
        let mut id = 0u64;
        loop {
            let request = tokio::select! {
                biased;
                _=state.stopped()=>break,
                request=receiver.recv()=>{let Some(request)=request else {break;};request},
                message=read(&mut socket)=>{
                    match message {
                        Ok(value) if value.get("id").is_none() && value.get("method").and_then(Value::as_str).is_some()=>continue,
                        Ok(_)=>state.stop(ControlFailure::Protocol),
                        Err(reason)=>state.stop(reason),
                    }
                    break;
                }
            };
            if request.is_closed() {
                continue;
            }
            if let Some(reason) = state.failure() {
                request.not_sent(reason);
                break;
            }
            match request {
                Request::Start {
                    thread,
                    text,
                    client,
                    reply,
                } => {
                    id += 1;
                    let request_body = json!({"id":id,"method":"turn/start","params":{"threadId":thread.to_string(),"clientUserMessageId":client.to_string(),"input":[{"type":"text","text":text,"text_elements":[]}]}});
                    let mut write_attempted = false;
                    let attempt = async {
                        write_attempted = true;
                        socket
                            .send(Message::Text(request_body.to_string().into()))
                            .await
                            .map_err(|_| ControlFailure::Transport)?;
                        let value = response(&mut socket, id).await?;
                        classify(value)
                    };
                    let outcome = tokio::select! {
                        biased;
                        _=state.stopped()=>Err(state.failure().unwrap_or(ControlFailure::Stopped)),
                        result=tokio::time::timeout(deadline, attempt)=>result.unwrap_or(Err(ControlFailure::Timeout)),
                    };
                    match outcome {
                        Ok(submission) => {
                            let _ = reply.send(submission);
                        }
                        Err(reason) => {
                            state.stop(reason);
                            let outcome = if write_attempted {
                                Submission::Unknown { reason }
                            } else {
                                Submission::NotSent { reason }
                            };
                            let _ = reply.send(outcome);
                            break;
                        }
                    }
                }
                Request::Answer {
                    id,
                    response,
                    validate,
                    reply,
                } => {
                    let body = Message::Text(response.envelope(&id).to_string().into());
                    let mut write_attempted = false;
                    let attempt = async {
                        // Wait for prior buffered writes before admitting this
                        // envelope. There is no await between the fresh gate and
                        // start_send; after admission bytes may be in flight.
                        std::future::poll_fn(|cx| std::pin::Pin::new(&mut socket).poll_ready(cx))
                            .await
                            .map_err(|_| ControlFailure::Transport)?;
                        if reply.is_closed() {
                            return Ok(ResponseWrite::NotSent {
                                reason: ControlFailure::Stopped,
                            });
                        }
                        if !validate() {
                            return Ok(ResponseWrite::NotSent {
                                reason: ControlFailure::InvalidInput,
                            });
                        }
                        write_attempted = true;
                        std::pin::Pin::new(&mut socket)
                            .start_send(body)
                            .map_err(|_| ControlFailure::Transport)?;
                        socket
                            .flush()
                            .await
                            .map_err(|_| ControlFailure::Transport)?;
                        // A result envelope has no response ACK. Flush success
                        // cannot prove native adoption or tool execution.
                        Ok(ResponseWrite::WrittenUnconfirmed)
                    };
                    let outcome = tokio::select! {
                        biased;
                        _=state.stopped()=>Err(state.failure().unwrap_or(ControlFailure::Stopped)),
                        result=tokio::time::timeout(deadline, attempt)=>result.unwrap_or(Err(ControlFailure::Timeout)),
                    };
                    match outcome {
                        Ok(outcome) => {
                            let _ = reply.send(outcome);
                        }
                        Err(reason) => {
                            state.stop(reason);
                            let outcome = if write_attempted {
                                ResponseWrite::Unknown { reason }
                            } else {
                                ResponseWrite::NotSent { reason }
                            };
                            let _ = reply.send(outcome);
                            break;
                        }
                    }
                }
            }
        }
        state.stop(ControlFailure::Stopped);
        receiver.close();
        while let Some(request) = receiver.recv().await {
            request.not_sent(state.failure().unwrap_or(ControlFailure::Stopped));
        }
    });
    (handle, task)
}
fn classify(value: Value) -> Result<Submission, ControlFailure> {
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => {
            let turn_id = result
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .filter(|id| !id.is_nil())
                .ok_or(ControlFailure::Protocol)?;
            Ok(Submission::Accepted { turn_id })
        }
        (None, Some(error)) => error
            .get("code")
            .and_then(Value::as_i64)
            .map(|code| Submission::Rejected { code })
            .ok_or(ControlFailure::Protocol),
        _ => Err(ControlFailure::Protocol),
    }
}
async fn response(socket: &mut Socket, id: u64) -> Result<Value, ControlFailure> {
    loop {
        let value = read(socket).await?;
        if value.get("id") == Some(&Value::from(id)) && value.get("method").is_none() {
            return Ok(value);
        }
        if value.get("id").is_some() || value.get("method").and_then(Value::as_str).is_none() {
            return Err(ControlFailure::Protocol);
        }
    }
}
async fn read(socket: &mut Socket) -> Result<Value, ControlFailure> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|_| ControlFailure::Protocol);
            }
            Some(Ok(Message::Ping(_))) => socket
                .flush()
                .await
                .map_err(|_| ControlFailure::Transport)?,
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Err(ControlFailure::Stopped),
            Some(Err(_)) => return Err(ControlFailure::Transport),
            _ => return Err(ControlFailure::Protocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::net::UnixListener;
    use tokio_tungstenite::accept_async;

    #[test]
    fn native_exit_requires_success_and_peer_gone_before_reap() {
        let sentinel = Process::new(
            Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let unrelated = sentinel.0.lock().unwrap().id() as i32;
        for (exit, peer_alive) in [(0, false), (7, false), (0, true)] {
            let process = Process::new(
                Command::new("/bin/sh")
                    .args(["-c", if exit == 0 { "exit 0" } else { "exit 7" }])
                    .process_group(0)
                    .spawn()
                    .unwrap(),
            );
            let pid = process.0.lock().unwrap().id() as i32;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while process.healthy() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(!process.healthy());
            assert_eq!(
                process.reap_normal(if peer_alive { unrelated } else { pid }),
                exit == 0 && !peer_alive
            );
            drop(process);
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            assert!(sentinel.healthy());
        }
        let signaled = Process::new(
            Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let pid = signaled.0.lock().unwrap().id() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while signaled.healthy() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!signaled.reap_normal(pid));
        drop(signaled);
        assert!(sentinel.healthy());
    }

    async fn pair() -> (Socket, Socket) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap()
        });
        let stream = UnixStream::connect(&path).await.unwrap();
        let client = tokio_tungstenite::client_async("ws://localhost/rpc", stream)
            .await
            .unwrap()
            .0;
        (client, server.await.unwrap())
    }
    fn socket_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, UnixListener) {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().canonicalize().unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("physical.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = parent.join("app.sock");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        (root, parent, alias, listener)
    }
    #[tokio::test]
    async fn native_alias_pins_inode_and_rejects_unrelated_peer_or_retarget() {
        let (_root, parent, alias, listener) = socket_fixture();
        let identity = SocketIdentity::resolve(&alias, &parent).unwrap();
        let stream = UnixStream::connect(&identity.path).await.unwrap();
        let _peer = listener.accept().await.unwrap();
        let group = unsafe { libc::getpgrp() } as u32;
        assert!(
            identity
                .verify_connected(&stream, group, &alias, &parent)
                .is_ok()
        );
        assert_eq!(
            identity.verify_connected(&stream, group + 1, &alias, &parent),
            Err(ControlFailure::StartupSocket)
        );
        let replacement = parent.join("replacement.sock");
        let _replacement = UnixListener::bind(&replacement).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(replacement, &alias).unwrap();
        assert_eq!(
            identity.verify_connected(&stream, group, &alias, &parent),
            Err(ControlFailure::StartupSocket)
        );
    }
    #[tokio::test]
    async fn native_alias_rejects_wrong_path_type_and_modes() {
        let (_root, parent, alias, _listener) = socket_fixture();
        let identity = SocketIdentity::resolve(&alias, &parent).unwrap();
        let alias_metadata = std::fs::symlink_metadata(&alias).unwrap();
        assert!(!valid_alias_metadata(
            &alias_metadata,
            alias_metadata.uid().wrapping_add(1)
        ));
        let wrong_alias = parent.join("other.sock");
        std::os::unix::fs::symlink(&identity.path, &wrong_alias).unwrap();
        assert!(SocketIdentity::resolve(&wrong_alias, &parent).is_err());
        std::fs::set_permissions(&identity.path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SocketIdentity::resolve(&alias, &parent).is_err());
        std::fs::set_permissions(&identity.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SocketIdentity::resolve(&alias, &parent).is_err());
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::fs::write(&alias, b"synthetic").unwrap();
        assert!(SocketIdentity::resolve(&alias, &parent).is_err());
    }
    #[tokio::test]
    async fn physical_socket_cleanup_never_removes_replacement_or_parent() {
        let (_root, parent, alias, _listener) = socket_fixture();
        let identity = SocketIdentity::resolve(&alias, &parent).unwrap();
        let path = identity.path.clone();
        std::fs::remove_file(&path).unwrap();
        let _replacement = UnixListener::bind(&path).unwrap();
        drop(OwnedSocket(identity));
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket()
        );
        assert!(parent.is_dir());
    }
    #[tokio::test]
    async fn typed_control_matches_id_and_only_sends_loaded_turn_start() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let thread = Uuid::new_v4();
        let client = Uuid::new_v4();
        let turn = Uuid::new_v4();
        let native = tokio::spawn(async move {
            let first = read(&mut server).await.unwrap();
            assert_eq!(first["method"], "turn/start");
            assert_eq!(first["params"]["threadId"], thread.to_string());
            assert_eq!(first["params"]["clientUserMessageId"], client.to_string());
            assert_eq!(first["params"]["input"][0]["text"], "synthetic");
            server
                .send(Message::Text(
                    json!({"id":first["id"],"error":{"code":-32600,"message":"PRIVATE_REJECTION"}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let second = read(&mut server).await.unwrap();
            assert_eq!(second["method"], "turn/start");
            assert_ne!(first["id"], second["id"]);
            server
                .send(Message::Text(
                    json!({"id":second["id"],"result":{"turn":{"id":turn}}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            server
        });
        assert_eq!(
            handle.turn_start(thread, "synthetic".into(), client).await,
            Submission::Rejected { code: -32600 }
        );
        assert_eq!(
            handle
                .turn_start(thread, "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::Accepted { turn_id: turn }
        );
        drop(native.await.unwrap());
        task.await.unwrap();
        assert!(handle.is_closed());
    }
    #[tokio::test]
    async fn wrong_ack_id_is_unknown_then_closed_without_replay() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let native = tokio::spawn(async move {
            let request = read(&mut server).await.unwrap();
            server.send(Message::Text(json!({"id":request["id"].as_u64().unwrap()+1,"result":{"turn":{"id":Uuid::new_v4()}}}).to_string().into())).await.unwrap();
            assert!(matches!(server.next().await, None | Some(Err(_))));
        });
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::Unknown {
                reason: ControlFailure::Protocol
            }
        );
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::NotSent {
                reason: ControlFailure::Protocol
            }
        );
        native.await.unwrap();
        task.await.unwrap();
    }
    #[tokio::test]
    async fn timeout_after_write_is_unknown_and_no_second_request_is_written() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_millis(30));
        let native = tokio::spawn(async move {
            assert_eq!(read(&mut server).await.unwrap()["method"], "turn/start");
            assert!(matches!(server.next().await, None | Some(Err(_))));
        });
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::Unknown {
                reason: ControlFailure::Timeout
            }
        );
        assert!(handle.is_closed());
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::NotSent {
                reason: ControlFailure::Timeout
            }
        );
        native.await.unwrap();
        task.await.unwrap();
    }
    #[tokio::test]
    async fn shutdown_and_invalid_input_before_write_are_not_sent() {
        let (socket, _server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        assert_eq!(
            handle
                .turn_start(Uuid::nil(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::NotSent {
                reason: ControlFailure::InvalidInput
            }
        );
        handle.state.stop(ControlFailure::Stopped);
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::NotSent {
                reason: ControlFailure::Stopped
            }
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn shutdown_distinguishes_in_flight_from_queued_and_busy() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let first_handle = handle.clone();
        let first = tokio::spawn(async move {
            first_handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await
        });
        assert_eq!(read(&mut server).await.unwrap()["method"], "turn/start");
        let (reply, queued) = oneshot::channel();
        assert!(
            handle
                .sender
                .try_send(Request::Start {
                    thread: Uuid::new_v4(),
                    text: "synthetic".into(),
                    client: Uuid::new_v4(),
                    reply
                })
                .is_ok()
        );
        assert_eq!(
            handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await,
            Submission::NotSent {
                reason: ControlFailure::Busy
            }
        );
        handle.state.stop(ControlFailure::Stopped);
        assert_eq!(
            first.await.unwrap(),
            Submission::Unknown {
                reason: ControlFailure::Stopped
            }
        );
        assert_eq!(
            queued.await.unwrap(),
            Submission::NotSent {
                reason: ControlFailure::Stopped
            }
        );
        assert_eq!(handle.closed().await, ControlFailure::Stopped);
        task.await.unwrap();
    }
    fn approval() -> NativeResponse {
        NativeResponse::Approval(crate::workbench::channel::interaction::Decision::Decline)
    }

    #[tokio::test]
    async fn typed_answers_preserve_id_types_and_do_not_wait_for_ack() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        for id in [RpcId::Number(7), RpcId::String("7".into())] {
            let expected = id.to_value();
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    handle.respond(id, approval(), Box::new(|| true)),
                )
                .await
                .unwrap(),
                ResponseWrite::WrittenUnconfirmed,
            );
            assert_eq!(
                read(&mut server).await.unwrap(),
                json!({"id":expected,"result":{"decision":"decline"}}),
            );
        }
        assert!(!handle.is_closed());
        handle.state.stop(ControlFailure::Stopped);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn stale_answer_guard_writes_zero_bytes_then_valid_answer_succeeds() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let called = calls.clone();
        assert_eq!(
            handle
                .respond(
                    RpcId::Number(7),
                    approval(),
                    Box::new(move || {
                        called.fetch_add(1, Ordering::SeqCst);
                        false
                    })
                )
                .await,
            ResponseWrite::NotSent {
                reason: ControlFailure::InvalidInput
            },
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.next())
                .await
                .is_err()
        );
        assert!(!handle.is_closed());
        assert_eq!(
            handle
                .respond(RpcId::Number(8), approval(), Box::new(|| true))
                .await,
            ResponseWrite::WrittenUnconfirmed,
        );
        assert_eq!(read(&mut server).await.unwrap()["id"], 8);
        handle.state.stop(ControlFailure::Stopped);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn dropped_answer_receiver_skips_guard_and_write() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let (reply, receiver) = oneshot::channel();
        assert!(
            handle
                .sender
                .try_send(Request::Answer {
                    id: RpcId::Number(7),
                    response: approval(),
                    validate: Box::new(|| panic!("dropped receiver must skip validation")),
                    reply,
                })
                .is_ok()
        );
        drop(receiver);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.next())
                .await
                .is_err()
        );
        assert_eq!(
            handle
                .respond(RpcId::Number(8), approval(), Box::new(|| true))
                .await,
            ResponseWrite::WrittenUnconfirmed,
        );
        assert_eq!(read(&mut server).await.unwrap()["id"], 8);
        handle.state.stop(ControlFailure::Stopped);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn answers_share_bounded_queue_and_shutdown_drains_typed_results() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let first_handle = handle.clone();
        let first = tokio::spawn(async move {
            first_handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await
        });
        assert_eq!(read(&mut server).await.unwrap()["method"], "turn/start");
        let (reply, receiver) = oneshot::channel();
        assert!(
            handle
                .sender
                .try_send(Request::Answer {
                    id: RpcId::Number(7),
                    response: approval(),
                    validate: Box::new(|| panic!("queued answer must not pass shutdown")),
                    reply,
                })
                .is_ok()
        );
        assert_eq!(
            handle
                .respond(RpcId::Number(8), approval(), Box::new(|| true))
                .await,
            ResponseWrite::NotSent {
                reason: ControlFailure::Busy
            },
        );
        handle.state.stop(ControlFailure::Stopped);
        assert_eq!(
            first.await.unwrap(),
            Submission::Unknown {
                reason: ControlFailure::Stopped
            }
        );
        assert_eq!(
            receiver.await.unwrap(),
            ResponseWrite::NotSent {
                reason: ControlFailure::Stopped
            }
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn answer_guard_rechecks_fresh_state_after_queue_wait() {
        let (socket, mut server) = pair().await;
        let (handle, task) = control(socket, Duration::from_secs(1));
        let first_handle = handle.clone();
        let first = tokio::spawn(async move {
            first_handle
                .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                .await
        });
        let start = read(&mut server).await.unwrap();
        let valid = Arc::new(AtomicBool::new(true));
        let guard = valid.clone();
        let (reply, receiver) = oneshot::channel();
        assert!(
            handle
                .sender
                .try_send(Request::Answer {
                    id: RpcId::Number(7),
                    response: approval(),
                    validate: Box::new(move || guard.load(Ordering::SeqCst)),
                    reply,
                })
                .is_ok()
        );
        valid.store(false, Ordering::SeqCst);
        server
            .send(Message::Text(
                json!({"id":start["id"],"error":{"code":-32600}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert_eq!(first.await.unwrap(), Submission::Rejected { code: -32600 });
        assert_eq!(
            receiver.await.unwrap(),
            ResponseWrite::NotSent {
                reason: ControlFailure::InvalidInput
            }
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), server.next())
                .await
                .is_err()
        );
        assert!(!handle.is_closed());
        handle.state.stop(ControlFailure::Stopped);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn answer_write_failure_is_unknown_and_stops_control() {
        use std::os::fd::AsRawFd;
        let (socket, _server) = pair().await;
        assert_eq!(
            unsafe { libc::shutdown(socket.get_ref().as_raw_fd(), libc::SHUT_WR) },
            0
        );
        let (handle, task) = control(socket, Duration::from_secs(1));
        assert_eq!(
            handle
                .respond(RpcId::Number(7), approval(), Box::new(|| true))
                .await,
            ResponseWrite::Unknown {
                reason: ControlFailure::Transport
            },
        );
        assert_eq!(
            handle
                .respond(RpcId::Number(8), approval(), Box::new(|| panic!("stopped")))
                .await,
            ResponseWrite::NotSent {
                reason: ControlFailure::Transport
            },
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn answer_write_timeout_is_unknown_without_replay() {
        // Fill the transport before wrapping it: a small typed answer must then
        // wait for writable capacity. The peer deliberately never reads.
        let (stream, _peer) = UnixStream::pair().unwrap();
        stream.writable().await.unwrap();
        loop {
            match stream.try_write(&[0; 8192]) {
                Ok(0) => panic!("unexpected zero-byte write"),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("synthetic socket fill failed: {error}"),
            }
        }
        let socket = WebSocketStream::from_raw_socket(
            stream,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let (handle, task) = control(socket, Duration::from_millis(30));
        assert_eq!(
            handle
                .respond(RpcId::Number(7), approval(), Box::new(|| true))
                .await,
            ResponseWrite::Unknown {
                reason: ControlFailure::Timeout
            },
        );
        assert_eq!(handle.failure(), Some(ControlFailure::Timeout));
        assert_eq!(
            handle
                .respond(RpcId::Number(8), approval(), Box::new(|| panic!("stopped")))
                .await,
            ResponseWrite::NotSent {
                reason: ControlFailure::Timeout
            },
        );
        task.await.unwrap();
    }

    #[test]
    fn malformed_and_ambiguous_ack_cannot_be_accepted() {
        for value in [
            json!({"result":{"turn":{"id":"invalid"}}}),
            json!({"result":{"turn":{"id":Uuid::nil()}}}),
            json!({"result":{"turn":{"id":Uuid::new_v4()}},"error":{"code":1}}),
            json!({"error":{"message":"PRIVATE"}}),
        ] {
            assert_eq!(classify(value), Err(ControlFailure::Protocol));
        }
    }
    #[tokio::test]
    #[ignore = "requires explicitly selected installed CLI; launches an isolated child test process"]
    async fn installed_cli_owner_isolated_lifecycle_and_loaded_only_control() {
        const CHILD: &str = "CODEX_CHANNEL_ISOLATED_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let binary =
                std::env::var_os("CODEX_CHANNEL_TEST_BINARY").expect("set explicit CLI path");
            let root = tempfile::Builder::new()
                .prefix("cv3-")
                .tempdir_in("/tmp")
                .unwrap();
            let home = root.path().join("home");
            let runtime_dir = root.path().join("runtime");
            std::fs::create_dir(&runtime_dir).unwrap();
            std::fs::create_dir(&home).unwrap();
            let child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["workbench::launch::channel::tests::installed_cli_owner_isolated_lifecycle_and_loaded_only_control", "--exact", "--ignored", "--nocapture"])
                .env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &home).env("USERPROFILE", &home).env("CODEX_HOME", &home).env("TMPDIR", &runtime_dir)
                .env(CHILD, "1").env("CODEX_CHANNEL_TEST_BINARY", binary)
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
            let output = tokio::time::timeout(Duration::from_secs(40), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if [
                    "CHANNEL_FIXTURE source=cli",
                    "CHANNEL_FIXTURE source=vscode",
                    "CHANNEL_FIXTURE source=exec",
                    "CHANNEL_FIXTURE source=appServer",
                    "CHANNEL_FIXTURE owned_primary_processes_reaped=true",
                ]
                .contains(&line)
                {
                    println!("{line}");
                }
            }
            for line in String::from_utf8_lossy(&output.stderr).lines() {
                if let Some(stage) = line.strip_prefix("CHANNEL_FIXTURE failure=")
                    && stage.len() <= 64
                    && stage.bytes().all(|b| b.is_ascii_alphabetic())
                {
                    println!("CHANNEL_FIXTURE failure={stage}");
                }
            }
            assert!(
                output.status.success(),
                "isolated native lifecycle child failed (private output suppressed)"
            );
            return;
        }
        let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
        let workspace = home.join("project");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!(
                r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[features]
daemon_auto_start=false
[model_providers.custom]
name="Synthetic fixture"
base_url="http://127.0.0.1:1/v1"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-only"
[projects.{}]
trust_level="trusted"
"#,
                toml::Value::String(workspace.display().to_string())
            ),
        )
        .unwrap();
        std::fs::create_dir(home.join("app-server-daemon")).unwrap();
        std::fs::write(
            home.join("app-server-daemon/settings.json"),
            r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
        )
        .unwrap();
        let binary = PathBuf::from(std::env::var_os("CODEX_CHANNEL_TEST_BINARY").unwrap());
        let upstream = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let proxy_url = format!("http://{}/v1", upstream.local_addr().unwrap());
        let mut checks = 0;
        let revalidation = super::super::WorkbenchRuntime::start_checked(
            super::super::LaunchOptions {
                channel_ready: true,
                cwd: workspace.clone(),
                home: home.clone(),
                workbench_paths: crate::workbench::paths::WorkbenchPaths::from_user_home(&home)
                    .unwrap(),
                executable: binary.clone(),
                native_profile: None,
                resume: None,
                entry_file: None,
                data_dir: Some(home.join("history")),
            },
            || {
                checks += 1;
                if checks == 2 {
                    anyhow::bail!("synthetic revalidation revoke");
                }
                Ok(())
            },
        )
        .await;
        assert_eq!(checks, 2);
        assert!(revalidation.is_err());
        tokio::time::timeout(Duration::from_secs(18), async {
            while std::fs::read_dir(std::env::var_os("TMPDIR").unwrap())
                .unwrap()
                .count()
                != 0
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("second validation failure must reclaim owned runtime directory");
        for unpublished in [false, true] {
            let (owner, mut attachment) =
                Owner::start(&binary, &home, &workspace, "custom", &proxy_url)
                    .await
                    .unwrap_or_else(|failure| {
                        eprintln!("CHANNEL_FIXTURE failure={failure:?}");
                        panic!("native owner startup failed");
                    });
            let peer = UnixStream::connect(owner.path().parent().unwrap().join("app.sock"))
                .await
                .unwrap();
            let native_pid = peer.peer_cred().unwrap().pid().unwrap();
            drop(peer);
            let parent = owner.path().parent().unwrap().to_path_buf();
            let physical_path = owner._physical_socket.0.path.clone();
            let pid = owner.process.0.lock().unwrap().id();
            assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o777, 0o700);
            assert_eq!(
                std::fs::metadata(owner.path()).unwrap().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(parent.join("app.sock")).unwrap().mode() & 0o777,
                0o600
            );
            assert!(matches!(
                attachment
                    .control
                    .turn_start(Uuid::new_v4(), "synthetic".into(), Uuid::new_v4())
                    .await,
                Submission::Rejected { code: -32600 }
            ));
            let mut command = portable_pty::CommandBuilder::new(&binary);
            command.cwd(&workspace);
            command.env("CODEX_HOME", &home);
            command.env("TERM", "xterm-256color");
            command.env("COLORTERM", "truecolor");
            command.args([
                "--remote".to_owned(),
                format!("unix://{}", owner.path().display()),
                "-c".into(),
                "tui.animations=false".into(),
            ]);
            let tui =
                crate::workbench::terminal::TerminalHost::spawn(Uuid::new_v4(), command, 45, 120)
                    .unwrap();
            let mut tracker = crate::workbench::channel::subscription::Tracker::new();
            let thread = tokio::time::timeout(Duration::from_secs(12), async {
                loop {
                    let frame = attachment.capture.recv().await.unwrap();
                    let state = attachment.capture.state();
                    assert!(
                        !state.closed
                            && state.gap_generation == 0
                            && state.connection_epoch == Some(frame.connection_epoch)
                    );
                    let value: Value = serde_json::from_slice(&frame.bytes).unwrap();
                    tracker.observe(frame.direction, &value).unwrap();
                    let targets = tracker.targets();
                    if let Some(target) = targets.first() {
                        let source = value["result"]["thread"]["source"].as_str().unwrap();
                        assert!(matches!(source, "cli" | "vscode" | "exec" | "appServer"));
                        println!("CHANNEL_FIXTURE source={source}");
                        return target.thread_id;
                    }
                }
            })
            .await
            .expect("TUI must qualify its own correlated native subscription");
            assert!(matches!(
                attachment
                    .control
                    .turn_start(thread, "synthetic".into(), Uuid::new_v4())
                    .await,
                Submission::Accepted { .. }
            ));
            let (_model_connection, _) =
                tokio::time::timeout(Duration::from_secs(5), upstream.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let handle = attachment.control.clone();
            drop(attachment);
            assert!(owner.healthy(), "observer drop must not stop native run");
            drop(tui);
            if unpublished {
                // Cancellation after native startup must not detach an unowned
                // reaper: dropping the unpublished Owner is itself a barrier.
                let (entered, ready) = oneshot::channel();
                let task = tokio::spawn(async move {
                    let _owner = owner;
                    let _ = entered.send(());
                    futures_util::future::pending::<()>().await;
                });
                ready.await.unwrap();
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                let shutdown = owner.begin_shutdown();
                assert!(handle.is_closed());
                tokio::time::timeout(Duration::from_secs(18), async {
                    while !shutdown.is_finished() {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("owned shutdown must be bounded");
                assert_eq!(shutdown.outcome(), Some(ShutdownOutcome::NativeExited));
            }
            assert!(handle.is_closed());
            assert!(!parent.exists());
            assert!(!physical_path.exists());
            assert!(
                physical_path.parent().unwrap().is_dir(),
                "official shared parent must remain"
            );
            tokio::time::timeout(Duration::from_secs(3), async {
                while unsafe { libc::kill(native_pid, 0) } == 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("native wrapper descendant must exit");
            println!("CHANNEL_FIXTURE owned_primary_processes_reaped=true");
            assert_ne!(
                unsafe { libc::kill(pid as i32, 0) },
                0,
                "owned child must be reaped"
            );
        }
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires explicit CLI; isolated long-running synthetic native tools"]
    async fn installed_cli_long_running_tool_shutdown_boundary() {
        const CHILD: &str = "CODEX_CHANNEL_TOOL_TEST_CHILD";
        const TEST: &str =
            "workbench::launch::channel::tests::installed_cli_long_running_tool_shutdown_boundary";
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::Builder::new()
                .prefix("ctool-")
                .tempdir_in("/tmp")
                .unwrap();
            let home = root.path().join("home");
            let runtime_dir = root.path().join("runtime");
            std::fs::create_dir(&home).unwrap();
            std::fs::create_dir(&runtime_dir).unwrap();
            let child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([TEST, "--exact", "--ignored", "--nocapture"])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("CODEX_HOME", &home)
                .env("TMPDIR", &runtime_dir)
                .env(CHILD, "1")
                .env(
                    "CODEX_CHANNEL_TEST_BINARY",
                    std::env::var_os("CODEX_CHANNEL_TEST_BINARY")
                        .expect("select explicit installed CLI"),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let output = tokio::time::timeout(Duration::from_secs(50), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if line.starts_with("TOOL_LIFECYCLE ")
                    && line.len() <= 400
                    && line
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b" =_-".contains(&b))
                {
                    println!("{line}");
                }
            }
            assert!(
                output.status.success(),
                "synthetic native tool shutdown regression failed; aggregate evidence above"
            );
            return;
        }
        fn process_info(pid: i32) -> Option<libc::proc_bsdinfo> {
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = std::mem::size_of_val(&info) as i32;
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            (read == size).then_some(info)
        }
        struct ToolIdentity {
            pid: i32,
            started: (u64, u64),
            group: u32,
        }
        impl ToolIdentity {
            fn same(&self) -> bool {
                process_info(self.pid).is_some_and(|info| {
                    info.pbi_uid == unsafe { libc::geteuid() }
                        && (info.pbi_start_tvsec, info.pbi_start_tvusec) == self.started
                        && info.pbi_pgid == self.group
                })
            }
            fn cleanup(&self) {
                if self.same() {
                    unsafe {
                        libc::kill(self.pid, libc::SIGKILL);
                    }
                }
            }
        }
        impl Drop for ToolIdentity {
            fn drop(&mut self) {
                self.cleanup();
            }
        }
        struct TaskGuard(JoinHandle<()>);
        impl Drop for TaskGuard {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let home = PathBuf::from(std::env::var_os("CODEX_HOME").unwrap());
        let workspace = home.join("project");
        std::fs::create_dir(&workspace).unwrap();
        let binary = PathBuf::from(std::env::var_os("CODEX_CHANNEL_TEST_BINARY").unwrap());
        std::fs::create_dir(home.join("app-server-daemon")).unwrap();
        std::fs::write(
            home.join("app-server-daemon/settings.json"),
            r#"{"remoteControlEnabled":false,"updater":{"autoUpdateEnabled":false}}"#,
        )
        .unwrap();
        let mut all_stopped = true;
        for (tty, forced) in [(false, false), (true, false), (false, true)] {
            let pid_file = workspace.join(if tty { "pty.pid" } else { "pipe.pid" });
            let escaped = pid_file.to_str().unwrap().replace('\'', "'\\''");
            let tool_command = format!("printf '%s' \"$$\" > '{escaped}'; exec /bin/sleep 30");
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let route = axum::Router::new().fallback(axum::routing::post(move || {
                let tool_command = tool_command.clone(); let calls = calls.clone();
                async move {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let item = if call == 0 { json!({"type":"function_call","id":"synthetic_tool","call_id":"synthetic_tool","name":"exec_command","arguments":json!({"cmd":tool_command,"login":false,"tty":tty,"yield_time_ms":1000,"max_output_tokens":100}).to_string()}) }
                        else { json!({"type":"message","id":"synthetic_final","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"SYNTHETIC"}]}) };
                    let events = [json!({"type":"response.created","response":{"id":"synthetic_response","model":"gpt-6-astra"}}), json!({"type":"response.output_item.added","output_index":0,"item":item}), json!({"type":"response.output_item.done","output_index":0,"item":item}), json!({"type":"response.completed","response":{"id":"synthetic_response"}})];
                    let text = events.into_iter().map(|value| format!("data: {value}\n\n")).collect::<String>();
                    axum::response::Response::builder().header("content-type", "text/event-stream").body(axum::body::Body::from(text)).unwrap()
                }
            }));
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let url = format!("http://{}/v1", listener.local_addr().unwrap());
            let _server = TaskGuard(tokio::spawn(async move {
                axum::serve(listener, route).await.unwrap();
            }));
            std::fs::write(
                home.join("config.toml"),
                format!(
                    r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="never"
sandbox_mode="workspace-write"
[features]
daemon_auto_start=false
[model_providers.custom]
name="Synthetic lifecycle fixture"
base_url="{url}"
wire_api="responses"
requires_openai_auth=false
experimental_bearer_token="synthetic-only"
[projects.{}]
trust_level="trusted"
"#,
                    toml::Value::String(workspace.display().to_string())
                ),
            )
            .unwrap();
            let sentinel = Process::new(
                Command::new("/bin/sleep")
                    .arg("30")
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let (owner, mut attachment) = Owner::start(&binary, &home, &workspace, "custom", &url)
                .await
                .unwrap();
            let owner_group = owner.process.0.lock().unwrap().id();
            let native_peer = owner.peer;
            let mut command = portable_pty::CommandBuilder::new(&binary);
            command.cwd(&workspace);
            command.env("CODEX_HOME", &home);
            command.env("TERM", "xterm-256color");
            command.args([
                "--remote".to_owned(),
                format!("unix://{}", owner.path().display()),
                "-c".into(),
                "tui.animations=false".into(),
            ]);
            let tui =
                crate::workbench::terminal::TerminalHost::spawn(Uuid::new_v4(), command, 45, 120)
                    .unwrap();
            let mut tracker = crate::workbench::channel::subscription::Tracker::new();
            let thread = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let frame = attachment.capture.recv().await.unwrap();
                    let state = attachment.capture.state();
                    assert!(
                        !state.closed
                            && state.gap_generation == 0
                            && state.connection_epoch == Some(frame.connection_epoch)
                    );
                    let value: Value = serde_json::from_slice(&frame.bytes).unwrap();
                    tracker.observe(frame.direction, &value).unwrap();
                    if let Some(target) = tracker.targets().first() {
                        break target.thread_id;
                    }
                }
            })
            .await
            .unwrap();
            let _turn = match attachment
                .control
                .turn_start(
                    thread,
                    "Run the synthetic lifecycle fixture".into(),
                    Uuid::new_v4(),
                )
                .await
            {
                Submission::Accepted { turn_id } => turn_id,
                _ => panic!("synthetic turn was not accepted"),
            };
            let tool = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Ok(contents) = std::fs::read_to_string(&pid_file)
                        && let Ok(pid) = contents.parse::<i32>()
                        && pid > 1
                        && let Some(info) = process_info(pid)
                    {
                        let command = info
                            .pbi_comm
                            .iter()
                            .take_while(|c| **c != 0)
                            .map(|c| *c as u8)
                            .collect::<Vec<_>>();
                        if command == b"sleep" && info.pbi_uid == unsafe { libc::geteuid() } {
                            break ToolIdentity {
                                pid,
                                started: (info.pbi_start_tvsec, info.pbi_start_tvusec),
                                group: info.pbi_pgid,
                            };
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("synthetic tool must start and publish its own PID");
            let outside_group = tool.group != owner_group;
            if forced {
                // Stop only our kernel-verified native peer. This forces the
                // production 15s deadline without changing the official binary.
                assert_eq!(unsafe { libc::getpgid(owner.peer) }, owner_group as i32);
                assert_eq!(unsafe { libc::kill(owner.peer, libc::SIGSTOP) }, 0);
            }
            drop(tui);
            let shutdown = owner.begin_shutdown();
            assert!(attachment.control.is_closed());
            tokio::time::timeout(Duration::from_secs(18), async {
                while !shutdown.is_finished() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("production owner shutdown must finish within its deadline");
            assert_eq!(
                shutdown.outcome(),
                Some(if forced {
                    ShutdownOutcome::ForcedUnknown
                } else {
                    ShutdownOutcome::NativeExited
                })
            );
            assert_eq!(unsafe { libc::kill(owner_group as i32, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            tokio::time::timeout(Duration::from_secs(3), async {
                while unsafe { libc::getpgid(native_peer) } == owner_group as i32 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("owned native peer must leave the owned group");
            drop(attachment);
            let tool_stopped = tokio::time::timeout(Duration::from_secs(3), async {
                while tool.same() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .is_ok();
            let unrelated_alive = sentinel.healthy();
            all_stopped &= tool_stopped != forced && unrelated_alive;
            tool.cleanup();
            let cleaned = tokio::time::timeout(Duration::from_secs(3), async {
                while tool.same() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .is_ok();
            println!(
                "TOOL_LIFECYCLE tty={tty} forced={forced} tool_outside_owner_group={outside_group} tool_stopped={tool_stopped} unrelated_alive={unrelated_alive} cleanup_confirmed={cleaned}"
            );
            assert!(cleaned && unrelated_alive);
        }
        assert!(
            all_stopped,
            "native lifecycle outcome or exact fallback ownership was incorrect"
        );
    }
}
