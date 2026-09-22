//! One serialized owner for native PTY input, authority, resize and VT state.
//! A dedicated thread drains the PTY independently of model forwarding/pages.
//! There is no shell fallback, database, automatic submission or input replay.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use tokio::sync::{mpsc as async_mpsc, oneshot};
use uuid::Uuid;

use super::control::{ControlError, ControlGrant, ControlView, InputControl};
use super::permission::Permission;
pub use super::terminal_screen::{OutputEvent, TerminalSnapshot};
use super::terminal_screen::{TerminalOutputFilter, TerminalReplyRequest};

mod screen;
use screen::ScreenJournal;

const CLIENT_EVENTS: usize = 64;
const COMMANDS: usize = 128;
const READ_BYTES: usize = 8192;
const GRACE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Busy,
    Unavailable,
    PtyReadFailed,
    ProtocolReplyFailed,
    ChildStatusFailed,
    ScreenStateUnavailable,
    Control(ControlError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalError {
    pub source: &'static str,
    pub run_epoch: Uuid,
    pub output_seq: Option<u64>,
    pub code: ErrorCode,
}
impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "terminal {:?} (epoch {})", self.code, self.run_epoch)
    }
}
impl std::error::Error for TerminalError {}
type Reply<T> = oneshot::Sender<std::result::Result<T, TerminalError>>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitView {
    pub code: u32,
    pub signal: Option<String>,
}

#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Output(OutputEvent),
    State {
        control: ControlView,
        exit: Option<ExitView>,
    },
    Fault(TerminalError),
}

pub struct Attachment {
    pub connection_id: Uuid,
    pub snapshot: TerminalSnapshot,
    pub control: ControlView,
    pub exit: Option<ExitView>,
    pub retained_bytes: usize,
    pub checkpoint_bytes: usize,
    pub fault: Option<TerminalError>,
    pub events: async_mpsc::Receiver<TerminalEvent>,
}

enum Command {
    Attach(Option<Permission>, Reply<Attachment>),
    RevokeAccess(Reply<()>),
    Grant {
        id: Uuid,
        action: GrantAction,
        reply: Reply<ControlGrant>,
    },
    Input {
        id: Uuid,
        generation: u64,
        sequence: u64,
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
    Resize {
        id: Uuid,
        generation: u64,
        rows: u16,
        cols: u16,
        reply: Reply<bool>,
    },
    Release {
        id: Uuid,
        generation: u64,
        reply: Reply<()>,
    },
    Stop(Reply<()>),
    #[cfg(test)]
    FailScreen(Reply<()>),
}
enum GrantAction {
    Claim,
    Takeover,
    Reconnect(String),
}

#[derive(Clone)]
pub struct TerminalHandle {
    commands: SyncSender<Command>,
    epoch: Uuid,
    process_id: u32,
    project_name: Arc<str>,
}
impl TerminalHandle {
    pub fn epoch(&self) -> Uuid {
        self.epoch
    }
    pub fn process_id(&self) -> u32 {
        self.process_id
    }
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    fn error(&self, code: ErrorCode) -> TerminalError {
        TerminalError {
            source: "workbench_terminal",
            run_epoch: self.epoch,
            output_seq: None,
            code,
        }
    }
    async fn call<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Command,
    ) -> std::result::Result<T, TerminalError> {
        let (reply, result) = oneshot::channel();
        self.commands.try_send(build(reply)).map_err(|error| {
            self.error(match error {
                mpsc::TrySendError::Full(_) => ErrorCode::Busy,
                mpsc::TrySendError::Disconnected(_) => ErrorCode::Unavailable,
            })
        })?;
        // The actor consumes accepted input even if the caller disconnects.
        // The client must not retry bytes whose acknowledgement was lost.
        result
            .await
            .map_err(|_| self.error(ErrorCode::Unavailable))?
    }
    pub async fn attach(&self) -> std::result::Result<Attachment, TerminalError> {
        self.attach_authorized(None).await
    }
    pub async fn attach_authorized(
        &self,
        permission: Option<Permission>,
    ) -> std::result::Result<Attachment, TerminalError> {
        self.call(|reply| Command::Attach(permission, reply)).await
    }
    pub async fn revoke_access(&self) -> std::result::Result<(), TerminalError> {
        self.call(Command::RevokeAccess).await
    }
    async fn grant(
        &self,
        id: Uuid,
        action: GrantAction,
    ) -> std::result::Result<ControlGrant, TerminalError> {
        self.call(|reply| Command::Grant { id, action, reply })
            .await
    }
    pub async fn claim(&self, id: Uuid) -> std::result::Result<ControlGrant, TerminalError> {
        self.grant(id, GrantAction::Claim).await
    }
    pub async fn takeover(&self, id: Uuid) -> std::result::Result<ControlGrant, TerminalError> {
        self.grant(id, GrantAction::Takeover).await
    }
    pub async fn reconnect(
        &self,
        id: Uuid,
        secret: String,
    ) -> std::result::Result<ControlGrant, TerminalError> {
        self.grant(id, GrantAction::Reconnect(secret)).await
    }
    pub async fn input(
        &self,
        id: Uuid,
        generation: u64,
        sequence: u64,
        bytes: Vec<u8>,
    ) -> std::result::Result<(), TerminalError> {
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err(self.error(ErrorCode::Control(ControlError::InputSize)));
        }
        self.call(|reply| Command::Input {
            id,
            generation,
            sequence,
            bytes,
            reply,
        })
        .await
    }
    pub async fn resize(
        &self,
        id: Uuid,
        generation: u64,
        rows: u16,
        cols: u16,
    ) -> std::result::Result<bool, TerminalError> {
        self.call(|reply| Command::Resize {
            id,
            generation,
            rows,
            cols,
            reply,
        })
        .await
    }
    pub async fn release(
        &self,
        id: Uuid,
        generation: u64,
    ) -> std::result::Result<(), TerminalError> {
        self.call(|reply| Command::Release {
            id,
            generation,
            reply,
        })
        .await
    }
    pub async fn stop(&self) -> std::result::Result<(), TerminalError> {
        self.call(Command::Stop).await
    }
}

pub struct TerminalHost {
    handle: TerminalHandle,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    process_id: u32,
}
impl TerminalHost {
    /// Only the local launcher supplies a validated ordinary CLI command.
    /// Browser requests never select the executable, argv, cwd or environment.
    pub fn spawn(epoch: Uuid, command: CommandBuilder, rows: u16, cols: u16) -> Result<Self> {
        let control = InputControl::new(rows, cols)
            .map_err(|_| anyhow::anyhow!("invalid initial terminal dimensions"))?;
        let project_name: Arc<str> = command
            .get_cwd()
            .and_then(|cwd| std::path::Path::new(cwd).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "当前项目".into())
            .into();
        let pty = Pty::spawn(command, rows, cols)?;
        let process_id = pty.pid;
        let (commands, receiver) = mpsc::sync_channel(COMMANDS);
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let thread = std::thread::Builder::new()
            .name("workbench-terminal".into())
            .spawn(move || {
                Actor {
                    epoch,
                    control,
                    pty,
                    journal: ScreenJournal::new(rows, cols),
                    filter: TerminalOutputFilter::default(),
                    clients: HashMap::new(),
                    last_generation: 0,
                    exit: None,
                    stopping: None,
                    read_closed: false,
                    fault: None,
                }
                .run(receiver, stop);
            })
            .context("start terminal actor")?;
        Ok(Self {
            handle: TerminalHandle {
                commands,
                epoch,
                process_id,
                project_name,
            },
            shutdown,
            thread: Some(thread),
            process_id,
        })
    }
    pub fn handle(&self) -> TerminalHandle {
        self.handle.clone()
    }
    pub fn process_id(&self) -> u32 {
        self.process_id
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
}
impl Drop for TerminalHost {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    fd: RawFd, // Borrowed from master; valid until master is dropped.
    pid: u32,
    reaped: bool,
}
impl Pty {
    fn spawn(command: CommandBuilder, rows: u16, cols: u16) -> Result<Self> {
        let pair = native_pty_system().openpty(size(rows, cols))?;
        let fd = pair.master.as_raw_fd().context("PTY requires a Unix fd")?;
        // SAFETY: fd is owned by the live master. Nonblocking I/O prevents a
        // child that stops reading from freezing control/cleanup indefinitely.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(flags >= 0, "cannot read PTY flags");
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "cannot make PTY nonblocking"
        );
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let Some(pid) = child.process_id() else {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("native child has no process identity");
        };
        Ok(Self {
            master: pair.master,
            child,
            fd,
            pid,
            reaped: false,
        })
    }
    fn signal(&mut self, signal: i32) {
        if self.reaped {
            return;
        }
        // portable-pty creates a new session before exec. Only signal the
        // group while its owned leader still exists; never use the foreground
        // group of an unrelated terminal or a pid supplied by a web request.
        let pid = self.pid as libc::pid_t;
        // SAFETY: positive pid belongs to this unreaped child. getpgid verifies
        // the session-leader assumption before a negative process-group target.
        unsafe {
            if pid > 1 && libc::getpgid(pid) == pid {
                libc::kill(-pid, signal);
            } else if pid > 1 {
                libc::kill(pid, signal);
            }
        }
    }
}
impl Drop for Pty {
    fn drop(&mut self) {
        if !self.reaped {
            self.signal(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}
fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

struct Actor {
    epoch: Uuid,
    control: InputControl,
    pty: Pty,
    journal: ScreenJournal,
    filter: TerminalOutputFilter,
    clients: HashMap<Uuid, async_mpsc::Sender<TerminalEvent>>,
    last_generation: u64,
    exit: Option<ExitView>,
    stopping: Option<Instant>,
    read_closed: bool,
    fault: Option<TerminalError>,
}
impl Actor {
    fn fault(&mut self, code: ErrorCode) {
        if self.fault.as_ref().is_some_and(|fault| fault.code == code) {
            return;
        }
        let fault = TerminalError {
            source: "workbench_terminal",
            run_epoch: self.epoch,
            output_seq: Some(self.journal.last_seq()),
            code,
        };
        self.fault = Some(fault.clone());
        self.broadcast(TerminalEvent::Fault(fault));
    }
    fn finish<T>(&self, reply: Reply<T>, value: std::result::Result<T, ControlError>) {
        let _ = reply.send(value.map_err(|code| TerminalError {
            source: "workbench_terminal",
            run_epoch: self.epoch,
            output_seq: Some(self.journal.last_seq()),
            code: ErrorCode::Control(code),
        }));
    }
    fn broadcast(&mut self, event: TerminalEvent) {
        let now = Instant::now();
        self.clients.retain(|id, sender| {
            if sender.try_send(event.clone()).is_ok() {
                true
            } else {
                self.control.disconnect(*id, now);
                false
            }
        });
    }
    fn publish_state(&mut self) {
        let control = self.control.view(Instant::now());
        self.last_generation = control.generation;
        self.broadcast(TerminalEvent::State {
            control,
            exit: self.exit.clone(),
        });
    }
    fn append(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let output = self.journal.append(bytes);
            self.check_screen();
            self.broadcast(TerminalEvent::Output(output));
        }
    }
    fn check_screen(&mut self) {
        if self.journal.failed() {
            self.fault(ErrorCode::ScreenStateUnavailable);
        }
    }
    fn output(&mut self, bytes: &[u8]) {
        let mut display = Vec::with_capacity(bytes.len());
        // Preserve query position within a chunk: a CPR before more output
        // must use the cursor at the query, not at the end of that chunk.
        for byte in bytes {
            let filtered = self.filter.process(std::slice::from_ref(byte));
            display.extend(filtered.display_bytes);
            if !filtered.terminal_replies.is_empty() {
                self.append(std::mem::take(&mut display));
                for query in filtered.terminal_replies {
                    let cursor = self.journal.cursor_position();
                    self.check_screen();
                    // An unavailable screen cannot truthfully report a cursor.
                    // Fixed capability/color replies remain available.
                    if query == TerminalReplyRequest::CursorPosition && cursor.is_none() {
                        continue;
                    }
                    let reply = query.encode(cursor.unwrap_or_default());
                    if write_input(self.pty.fd, &reply).is_err() {
                        self.fault(ErrorCode::ProtocolReplyFailed);
                    }
                }
            }
        }
        self.append(display);
    }
    fn drain(&mut self) {
        if self.read_closed {
            return;
        }
        let mut bytes = [0; READ_BYTES];
        for _ in 0..16 {
            // SAFETY: master keeps fd valid, bytes is writable for its length.
            let count = unsafe { libc::read(self.pty.fd, bytes.as_mut_ptr().cast(), bytes.len()) };
            if count > 0 {
                self.output(&bytes[..count as usize]);
            } else if count == 0 {
                self.read_closed = true;
                break;
            } else {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.kind() != io::ErrorKind::WouldBlock {
                    self.read_closed = true;
                    // Linux PTYs report slave closure as EIO, not read(0).
                    if error.raw_os_error() != Some(libc::EIO) {
                        self.fault(ErrorCode::PtyReadFailed);
                    }
                }
                break;
            }
        }
    }
    fn stop(&mut self) {
        if self.stopping.is_none() && self.exit.is_none() {
            self.stopping = Some(Instant::now());
            self.control.end();
            self.pty.signal(libc::SIGTERM);
            self.publish_state();
        }
    }
    fn prune_access(&mut self) {
        for id in self.control.prune_revoked() {
            self.clients.remove(&id);
        }
    }
    fn command(&mut self, command: Command) {
        self.prune_access();
        match command {
            Command::Attach(permission, reply) => {
                let result = self.control.connect_authorized(permission).map(|id| {
                    let (sender, events) = async_mpsc::channel(CLIENT_EVENTS);
                    // Snapshot and registration happen in the same actor step;
                    // the next output cannot fall between the two operations.
                    let snapshot = self.journal.snapshot();
                    self.check_screen();
                    self.clients.insert(id, sender);
                    Attachment {
                        connection_id: id,
                        snapshot,
                        control: self.control.view(Instant::now()),
                        exit: self.exit.clone(),
                        retained_bytes: self.journal.retained_bytes(),
                        checkpoint_bytes: self.journal.checkpoint_bytes(),
                        fault: self.fault.clone(),
                        events,
                    }
                });
                self.finish(reply, result);
            }
            Command::Grant { id, action, reply } => {
                let result = match action {
                    GrantAction::Claim => self.control.claim(id, Instant::now()),
                    GrantAction::Takeover => self.control.takeover(id),
                    GrantAction::Reconnect(secret) => {
                        self.control.reconnect(id, &secret, Instant::now())
                    }
                };
                self.finish(reply, result);
                self.publish_state();
            }
            Command::Input {
                id,
                generation,
                sequence,
                bytes,
                reply,
            } => {
                let result = self
                    .control
                    .input(id, generation, sequence, &bytes, |bytes| {
                        write_input(self.pty.fd, bytes)
                    });
                self.finish(reply, result);
            }
            Command::Resize {
                id,
                generation,
                rows,
                cols,
                reply,
            } => {
                let result = self.control.resize(id, generation, rows, cols, |r, c| {
                    self.pty.master.resize(size(r, c)).map_err(io::Error::other)
                });
                if result == Ok(true) {
                    self.journal.resize(rows, cols);
                    self.check_screen();
                    self.publish_state();
                }
                self.finish(reply, result);
            }
            Command::Release {
                id,
                generation,
                reply,
            } => {
                let result = self.control.release(id, generation);
                self.finish(reply, result);
                self.publish_state();
            }
            Command::RevokeAccess(reply) => {
                self.prune_access();
                self.publish_state();
                self.finish(reply, Ok(()));
            }
            Command::Stop(reply) => {
                self.stop();
                self.finish(reply, Ok(()));
            }
            #[cfg(test)]
            Command::FailScreen(reply) => {
                self.journal.fail_for_test();
                self.check_screen();
                self.finish(reply, Ok(()));
            }
        }
    }
    fn run(&mut self, commands: Receiver<Command>, shutdown: Arc<AtomicBool>) {
        loop {
            let dropping = shutdown.load(Ordering::Acquire);
            if dropping {
                self.stop();
            }
            self.prune_access();
            let now = Instant::now();
            self.clients.retain(|id, sender| {
                if sender.is_closed() {
                    self.control.disconnect(*id, now);
                    false
                } else {
                    true
                }
            });
            if self.control.view(now).generation != self.last_generation {
                self.publish_state();
            }
            // Bound both command work and output work so neither can starve the
            // other. A slow page only loses its subscription, never PTY bytes.
            for _ in 0..16 {
                match commands.try_recv() {
                    Ok(command) => self.command(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.stop();
                        break;
                    }
                }
            }
            self.drain();
            if self.exit.is_none() {
                if self.stopping.is_some_and(|at| at.elapsed() >= GRACE) {
                    self.pty.signal(libc::SIGKILL);
                }
                match self.pty.child.try_wait() {
                    Ok(Some(status)) => {
                        self.pty.reaped = true;
                        self.exit = Some(ExitView {
                            code: status.exit_code(),
                            signal: status.signal().map(str::to_owned),
                        });
                        self.control.end();
                        self.publish_state();
                    }
                    Ok(None) => {}
                    Err(_) => self.fault(ErrorCode::ChildStatusFailed),
                }
            }
            if dropping && self.exit.is_some() {
                self.drain();
                break;
            }
            if self.read_closed {
                std::thread::sleep(Duration::from_millis(10));
            } else {
                let mut poll = libc::pollfd {
                    fd: self.pty.fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: live fd, one valid pollfd; short timeout also wakes
                // command processing when the native CLI has no output.
                unsafe { libc::poll(&mut poll, 1, 10) };
            }
        }
    }
}

fn write_input(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(100);
    while !bytes.is_empty() {
        // SAFETY: actor owns the live fd and bytes remains valid for this call.
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            bytes = &bytes[count as usize..];
        } else if count == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: same live fd; bounded wait never holds model forwarding.
            unsafe { libc::poll(&mut poll, 1, 5) };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
