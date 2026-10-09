//! One-shot, in-memory observation of one real control answer write. Test/macOS only.
use super::{ResponseWrite, RpcId};
use crate::workbench::channel::native::Direction;
use serde_json::Value;
use std::{
    io::{self, IoSlice},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixStream,
    sync::Notify,
};

#[derive(Clone, Copy, Default)]
pub(crate) struct AnswerWriteSnapshot {
    pub admitted: usize,
    pub blocked_writes: usize,
    pub bytes_written: usize,
    pub flushed: usize,
    pub written_unconfirmed: usize,
    pub not_sent: usize,
    pub unknown: usize,
    pub tui_accept: usize,
    pub tui_other: usize,
    pub resolved: usize,
    pub released: bool,
    pub closed: bool,
}
#[derive(Default)]
struct Data {
    snapshot: AnswerWriteSnapshot,
    armed: bool,
    entered: bool,
    active: bool,
    finished: bool,
    // Exact typed identity only. Never retain a request, response or text body.
    id: Option<RpcId>,
    writer: Option<Waker>,
}
#[derive(Default)]
struct Shared {
    data: Mutex<Data>,
    changed: Notify,
}
#[derive(Clone, Default)]
pub(crate) struct AnswerWriteProbe(Arc<Shared>);
pub(crate) struct AnswerWriteGate(AnswerWriteProbe);
impl AnswerWriteGate {
    pub(crate) fn release(&self) {
        self.0.release();
    }
}
impl Drop for AnswerWriteGate {
    fn drop(&mut self) {
        self.release();
    }
}
impl AnswerWriteProbe {
    pub(crate) fn arm(&self) -> Result<AnswerWriteGate, &'static str> {
        let mut data = self.0.data.lock().unwrap();
        if data.snapshot.closed {
            return Err("answer transport closed");
        }
        if data.armed {
            return Err("answer probe already armed");
        }
        data.armed = true;
        Ok(AnswerWriteGate(self.clone()))
    }
    pub(crate) fn snapshot(&self) -> AnswerWriteSnapshot {
        self.0.data.lock().unwrap().snapshot
    }
    pub(crate) async fn wait_for(
        &self,
        predicate: impl Fn(&AnswerWriteSnapshot) -> bool,
    ) -> AnswerWriteSnapshot {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let snapshot = self.snapshot();
            if predicate(&snapshot) {
                return snapshot;
            }
            changed.await;
        }
    }
    fn release(&self) {
        let writer = {
            let mut data = self.0.data.lock().unwrap();
            data.snapshot.released = true;
            data.writer.take()
        };
        self.0.changed.notify_waiters();
        if let Some(writer) = writer {
            writer.wake();
        }
    }
    pub(super) fn begin(&self) -> bool {
        let mut data = self.0.data.lock().unwrap();
        if !data.armed || data.entered || data.snapshot.closed {
            return false;
        }
        data.entered = true;
        true
    }
    pub(super) fn activate(&self, id: &RpcId) {
        let mut data = self.0.data.lock().unwrap();
        data.id = Some(id.clone());
        data.active = true;
    }
    pub(super) fn admitted(&self) {
        self.0.data.lock().unwrap().snapshot.admitted += 1;
        self.0.changed.notify_waiters();
    }
    pub(super) fn flushed(&self) {
        self.0.data.lock().unwrap().snapshot.flushed += 1;
        self.0.changed.notify_waiters();
    }
    pub(super) fn finish(&self, outcome: ResponseWrite) {
        let writer = {
            let mut data = self.0.data.lock().unwrap();
            if !data.finished {
                match outcome {
                    ResponseWrite::WrittenUnconfirmed => data.snapshot.written_unconfirmed += 1,
                    ResponseWrite::NotSent { .. } => data.snapshot.not_sent += 1,
                    ResponseWrite::Unknown { .. } => data.snapshot.unknown += 1,
                }
                data.finished = true;
                data.active = false;
            }
            data.writer.take()
        };
        self.0.changed.notify_waiters();
        if let Some(writer) = writer {
            writer.wake();
        }
    }
    pub(super) fn close(&self) {
        let writer = {
            let mut data = self.0.data.lock().unwrap();
            data.snapshot.closed = true;
            data.writer.take()
        };
        self.0.changed.notify_waiters();
        if let Some(writer) = writer {
            writer.wake();
        }
    }
    fn abandon(&self) {
        self.close();
        let outcome = {
            let data = self.0.data.lock().unwrap();
            if !data.entered || data.finished {
                return;
            }
            if data.active {
                ResponseWrite::Unknown {
                    reason: super::ControlFailure::Stopped,
                }
            } else {
                ResponseWrite::NotSent {
                    reason: super::ControlFailure::Stopped,
                }
            }
        };
        self.finish(outcome);
    }
    /// Observe only metadata matching the one original typed remote request ID.
    /// A TUI result observation proves emission, not upstream write completion.
    pub(crate) fn observe(&self, direction: Direction, value: &Value) {
        let mut data = self.0.data.lock().unwrap();
        let Some(id) = data.id.as_ref() else { return };
        match direction {
            Direction::ToServer
                if value.get("method").is_none()
                    && value
                        .get("id")
                        .and_then(|value| RpcId::parse(value).ok())
                        .as_ref()
                        == Some(id)
                    && (value.get("result").is_some() || value.get("error").is_some()) =>
            {
                if value.pointer("/result/decision").and_then(Value::as_str) == Some("accept") {
                    data.snapshot.tui_accept += 1;
                } else {
                    data.snapshot.tui_other += 1;
                }
            }
            Direction::ToTui
                if value.get("method").and_then(Value::as_str)
                    == Some("serverRequest/resolved")
                    && value
                        .pointer("/params/requestId")
                        .and_then(|value| RpcId::parse(value).ok())
                        .as_ref()
                        == Some(id) =>
            {
                data.snapshot.resolved += 1;
            }
            _ => return,
        }
        drop(data);
        self.0.changed.notify_waiters();
    }

    fn poll_write(
        &self,
        cx: &mut Context<'_>,
        write: impl FnOnce(&mut Context<'_>) -> Poll<io::Result<usize>>,
    ) -> Poll<io::Result<usize>> {
        // The synchronous syscall poll is serialized with close/release, never
        // an await. Register on every blocked poll: tungstenite's start_send can
        // carry an earlier proxy waker, subsequently refreshed by poll_flush.
        let mut data = self.0.data.lock().unwrap();
        if data.snapshot.closed {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if data.active && !data.snapshot.released {
            data.snapshot.blocked_writes += 1;
            data.writer = Some(cx.waker().clone());
            drop(data);
            self.0.changed.notify_waiters();
            return Poll::Pending;
        }
        let result = write(cx);
        if data.active
            && let Poll::Ready(Ok(count)) = &result
        {
            data.snapshot.bytes_written += *count;
        }
        drop(data);
        self.0.changed.notify_waiters();
        result
    }
}
pub(super) struct CloseOnDrop(pub Option<AnswerWriteProbe>);
impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        if let Some(probe) = &self.0 {
            probe.abandon();
        }
    }
}
pub(super) struct ProbeStream {
    inner: UnixStream,
    pub(super) probe: AnswerWriteProbe,
}
impl ProbeStream {
    pub(super) fn new(inner: UnixStream) -> Self {
        Self {
            inner,
            probe: AnswerWriteProbe::default(),
        }
    }
}
impl std::ops::Deref for ProbeStream {
    type Target = UnixStream;
    fn deref(&self) -> &UnixStream {
        &self.inner
    }
}
impl AsyncRead for ProbeStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for ProbeStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.probe
            .poll_write(cx, |cx| Pin::new(&mut this.inner).poll_write(cx, buf))
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.probe.poll_write(cx, |cx| {
            Pin::new(&mut this.inner).poll_write_vectored(cx, bufs)
        })
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ControlFailure, NativeResponse, control_probed, read};
    use super::*;
    use crate::workbench::channel::interaction::Decision;
    use serde_json::json;
    use std::{os::fd::AsRawFd, time::Duration};
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    async fn fixture(
        deadline: Duration,
    ) -> (
        super::super::ControlHandle,
        tokio::task::JoinHandle<()>,
        UnixStream,
        AnswerWriteProbe,
    ) {
        let (client, peer) = UnixStream::pair().unwrap();
        let stream = ProbeStream::new(client);
        let probe = stream.probe.clone();
        let socket = WebSocketStream::from_raw_socket(stream, Role::Client, None).await;
        let (handle, task) = control_probed(socket, deadline);
        (handle, task, peer, probe)
    }
    fn no_bytes(peer: &UnixStream) {
        let mut buffer = [0u8; 256];
        // Inspect the kernel directly: Tokio's cached readiness can initially
        // report WouldBlock without attempting a syscall, a weaker assertion.
        let result = unsafe {
            libc::recv(
                peer.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        assert_eq!(result, -1);
        assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
    }
    async fn blocked(probe: &AnswerWriteProbe) -> AnswerWriteSnapshot {
        tokio::time::timeout(
            Duration::from_secs(1),
            probe.wait_for(|s| s.admitted == 1 && s.blocked_writes > 0),
        )
        .await
        .unwrap()
    }
    fn answer() -> NativeResponse {
        NativeResponse::Approval(Decision::Decline)
    }

    #[tokio::test]
    async fn admitted_answer_has_zero_peer_bytes_until_real_write_release() {
        for id in [RpcId::Number(7), RpcId::String("7".into())] {
            let (handle, task, peer, probe) = fixture(Duration::from_secs(2)).await;
            let gate = probe.arm().unwrap();
            assert!(probe.arm().is_err());
            let expected = id.to_value();
            let writer = handle.clone();
            let reply =
                tokio::spawn(async move { writer.respond(id, answer(), Box::new(|| true)).await });
            let snapshot = blocked(&probe).await;
            assert_eq!(snapshot.bytes_written, 0);
            assert_eq!(snapshot.flushed, 0);
            assert_eq!(snapshot.written_unconfirmed, 0);
            no_bytes(&peer);
            // A string ID must not match a numeric ID, or vice versa.
            let other = if expected.is_string() {
                json!(7)
            } else {
                json!("7")
            };
            probe.observe(
                Direction::ToServer,
                &json!({"id":other,"result":{"decision":"accept"}}),
            );
            assert_eq!(probe.snapshot().tui_accept, 0);
            probe.observe(
                Direction::ToServer,
                &json!({"id":expected,"result":{"decision":"accept"}}),
            );
            probe.observe(
                Direction::ToTui,
                &json!({"method":"serverRequest/resolved","params":{"requestId":expected}}),
            );
            assert_eq!(probe.snapshot().tui_accept, 1);
            assert_eq!(probe.snapshot().resolved, 1);
            gate.release();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), reply)
                    .await
                    .unwrap()
                    .unwrap(),
                ResponseWrite::WrittenUnconfirmed
            );
            let mut server = WebSocketStream::from_raw_socket(peer, Role::Server, None).await;
            assert_eq!(
                read(&mut server).await.unwrap(),
                json!({"id":expected,"result":{"decision":"decline"}})
            );
            let snapshot = probe.snapshot();
            assert!(snapshot.bytes_written > 0);
            assert_eq!(snapshot.flushed, 1);
            assert_eq!(snapshot.written_unconfirmed, 1);
            assert_eq!(snapshot.unknown + snapshot.not_sent, 0);
            handle.state.stop(ControlFailure::Stopped);
            task.await.unwrap();
            assert!(probe.snapshot().closed);
            assert!(probe.arm().is_err());
        }
    }

    #[tokio::test]
    async fn stopped_or_expired_admitted_answer_is_unknown_without_replay() {
        for expire in [false, true] {
            let deadline = if expire {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(2)
            };
            let (handle, task, peer, probe) = fixture(deadline).await;
            let gate = probe.arm().unwrap();
            let writer = handle.clone();
            let reply = tokio::spawn(async move {
                writer
                    .respond(RpcId::Number(9), answer(), Box::new(|| true))
                    .await
            });
            blocked(&probe).await;
            no_bytes(&peer);
            if !expire {
                handle.state.stop(ControlFailure::Stopped);
            }
            let reason = if expire {
                ControlFailure::Timeout
            } else {
                ControlFailure::Stopped
            };
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), reply)
                    .await
                    .unwrap()
                    .unwrap(),
                ResponseWrite::Unknown { reason }
            );
            task.await.unwrap();
            gate.release();
            let mut buffer = [0; 256];
            peer.readable().await.unwrap();
            assert_eq!(peer.try_read(&mut buffer).unwrap(), 0);
            let snapshot = probe.snapshot();
            assert_eq!(snapshot.admitted, 1);
            assert_eq!(
                snapshot.bytes_written
                    + snapshot.flushed
                    + snapshot.written_unconfirmed
                    + snapshot.not_sent,
                0
            );
            assert_eq!(snapshot.unknown, 1);
            assert!(snapshot.closed);
            assert_eq!(
                handle
                    .respond(RpcId::Number(9), answer(), Box::new(|| true))
                    .await,
                ResponseWrite::NotSent { reason }
            );
            assert_eq!(probe.snapshot().unknown, 1);
        }
    }

    #[tokio::test]
    async fn task_drop_closes_pending_write_and_gate_drop_releases_transport() {
        let (handle, task, peer, probe) = fixture(Duration::from_secs(2)).await;
        let gate = probe.arm().unwrap();
        let writer = handle.clone();
        let reply = tokio::spawn(async move {
            writer
                .respond(RpcId::Number(1), answer(), Box::new(|| true))
                .await
        });
        blocked(&probe).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            reply.await.unwrap(),
            ResponseWrite::Unknown {
                reason: ControlFailure::Stopped
            }
        );
        drop(gate);
        let snapshot = probe.snapshot();
        assert!(snapshot.closed && snapshot.released);
        assert_eq!(snapshot.unknown, 1);
        assert_eq!(snapshot.bytes_written, 0);
        let mut buffer = [0; 1];
        peer.readable().await.unwrap();
        assert_eq!(peer.try_read(&mut buffer).unwrap(), 0);
    }

    #[tokio::test]
    async fn vectored_write_cannot_bypass_gate_and_counts_only_written_bytes() {
        let (client, peer) = UnixStream::pair().unwrap();
        let mut stream = ProbeStream::new(client);
        let probe = stream.probe.clone();
        let gate = probe.arm().unwrap();
        assert!(probe.begin());
        probe.activate(&RpcId::Number(2));
        let chunks = [IoSlice::new(b"abc"), IoSlice::new(b"def")];
        let mut write = Box::pin(stream.write_vectored(&chunks));
        assert!(futures_util::poll!(&mut write).is_pending());
        assert!(probe.snapshot().blocked_writes > 0);
        assert_eq!(probe.snapshot().bytes_written, 0);
        no_bytes(&peer);
        drop(gate);
        let count = tokio::time::timeout(Duration::from_secs(1), write)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 6);
        let mut buffer = [0; 6];
        peer.readable().await.unwrap();
        assert_eq!(peer.try_read(&mut buffer).unwrap(), 6);
        assert_eq!(&buffer, b"abcdef");
        assert_eq!(probe.snapshot().bytes_written, 6);
    }

    #[tokio::test]
    async fn rejected_fresh_guard_never_activates_or_writes() {
        let (handle, task, peer, probe) = fixture(Duration::from_secs(2)).await;
        let _gate = probe.arm().unwrap();
        assert_eq!(
            handle
                .respond(RpcId::Number(1), answer(), Box::new(|| false))
                .await,
            ResponseWrite::NotSent {
                reason: ControlFailure::InvalidInput
            }
        );
        no_bytes(&peer);
        let snapshot = probe.snapshot();
        assert_eq!(snapshot.not_sent, 1);
        assert_eq!(
            snapshot.admitted + snapshot.blocked_writes + snapshot.bytes_written + snapshot.unknown,
            0
        );
        handle.state.stop(ControlFailure::Stopped);
        task.await.unwrap();
    }
}
