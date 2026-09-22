//! Bounded, non-blocking copies of model traffic. Payloads never implement Debug.
//!
//! This queue is before decoding/redaction. Its bytes are transient and must not
//! be sent to a browser, recorder, or diagnostic log directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::body::Bytes;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Request,
    Response,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Http,
    WebSocket,
}

// Decoder hints are still untrusted input. Keep the whole source out of generic
// Debug/Serialize paths just like raw body observations.
#[derive(Clone)]
pub struct Source {
    pub request_id: Uuid,
    pub direction: Direction,
    pub transport: Transport,
    /// A decoder hint only. No other HTTP headers enter the observation queue.
    pub content_type: String,
    pub content_encoding: String,
}

pub struct Observation {
    pub source: Arc<Source>,
    pub sequence: u64,
    pub received_at: Instant,
    pub bytes: Bytes,
    pub end: Option<StreamEnd>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEnd {
    Complete,
    Interrupted,
}

struct Budget {
    permits: Arc<Semaphore>,
    limit: usize,
    dropped_chunks: AtomicU64,
    dropped_bytes: AtomicU64,
    interrupted_streams: AtomicU64,
    transports: [AtomicU64; 5],
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TransportStats {
    pub http_requests: u64,
    pub http_responses: u64,
    pub sse_responses: u64,
    pub websocket_requests: u64,
    pub websocket_responses: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CaptureStats {
    pub retained_bytes: usize,
    pub byte_limit: usize,
    pub dropped_chunks: u64,
    pub dropped_bytes: u64,
    pub interrupted_streams: u64,
    pub transports: TransportStats,
}

#[derive(Clone)]
pub struct CaptureSender {
    sender: mpsc::Sender<Observation>,
    budget: Arc<Budget>,
}

pub struct CaptureReceiver {
    receiver: mpsc::Receiver<Observation>,
    budget: Arc<Budget>,
}

pub struct CaptureStream {
    sender: CaptureSender,
    source: Arc<Source>,
    sequence: u64,
    finished: bool,
}

pub fn channel(byte_limit: usize, chunk_limit: usize) -> (CaptureSender, CaptureReceiver) {
    assert!(byte_limit > 0 && byte_limit <= u32::MAX as usize);
    assert!(chunk_limit > 0);
    let (sender, receiver) = mpsc::channel(chunk_limit);
    let budget = Arc::new(Budget {
        permits: Arc::new(Semaphore::new(byte_limit)),
        limit: byte_limit,
        dropped_chunks: AtomicU64::new(0),
        dropped_bytes: AtomicU64::new(0),
        interrupted_streams: AtomicU64::new(0),
        transports: std::array::from_fn(|_| AtomicU64::new(0)),
    });
    (
        CaptureSender {
            sender,
            budget: budget.clone(),
        },
        CaptureReceiver { receiver, budget },
    )
}

impl Budget {
    fn stats(&self) -> CaptureStats {
        CaptureStats {
            retained_bytes: self.limit - self.permits.available_permits(),
            byte_limit: self.limit,
            dropped_chunks: self.dropped_chunks.load(Ordering::Relaxed),
            dropped_bytes: self.dropped_bytes.load(Ordering::Relaxed),
            interrupted_streams: self.interrupted_streams.load(Ordering::Relaxed),
            transports: TransportStats {
                http_requests: self.transports[0].load(Ordering::Relaxed),
                http_responses: self.transports[1].load(Ordering::Relaxed),
                sse_responses: self.transports[2].load(Ordering::Relaxed),
                websocket_requests: self.transports[3].load(Ordering::Relaxed),
                websocket_responses: self.transports[4].load(Ordering::Relaxed),
            },
        }
    }

    fn drop_chunk(&self, bytes: usize) {
        // Gap counters are out-of-band: a full queue cannot hide capture loss.
        self.dropped_chunks.fetch_add(1, Ordering::Relaxed);
        self.dropped_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

impl CaptureSender {
    pub fn stream(&self, source: Source) -> CaptureStream {
        let counter = match (source.transport, source.direction) {
            (Transport::Http, Direction::Request) => 0,
            (Transport::Http, Direction::Response) => 1,
            (Transport::WebSocket, Direction::Request) => 3,
            (Transport::WebSocket, Direction::Response) => 4,
        };
        self.budget.transports[counter].fetch_add(1, Ordering::Relaxed);
        if counter == 1
            && source
                .content_type
                .split(';')
                .next()
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/event-stream"))
        {
            self.budget.transports[2].fetch_add(1, Ordering::Relaxed);
        }
        CaptureStream {
            sender: self.clone(),
            source: Arc::new(source),
            sequence: 0,
            finished: false,
        }
    }

    pub fn stats(&self) -> CaptureStats {
        self.budget.stats()
    }
}

impl CaptureReceiver {
    pub async fn recv(&mut self) -> Option<Observation> {
        self.receiver.recv().await
    }

    pub fn stats(&self) -> CaptureStats {
        self.budget.stats()
    }
}

impl CaptureStream {
    pub fn offer(&mut self, bytes: Bytes, received_at: Instant) {
        if !self.finished {
            self.offer_inner(bytes, received_at, None);
        }
    }

    pub fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            self.offer_inner(Bytes::new(), Instant::now(), Some(StreamEnd::Complete));
        }
    }

    fn offer_inner(&mut self, bytes: Bytes, received_at: Instant, end: Option<StreamEnd>) {
        self.sequence += 1;
        let size = bytes.len();
        let Ok(permit_count) = u32::try_from(size) else {
            self.sender.budget.drop_chunk(size);
            return;
        };
        let Ok(permit) = self
            .sender
            .budget
            .permits
            .clone()
            .try_acquire_many_owned(permit_count)
        else {
            self.sender.budget.drop_chunk(size);
            return;
        };
        let observation = Observation {
            source: self.source.clone(),
            sequence: self.sequence,
            received_at,
            bytes,
            end,
            _permit: permit,
        };
        if self.sender.sender.try_send(observation).is_err() {
            self.sender.budget.drop_chunk(size);
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        if !self.finished {
            // Cancellation, body errors and early shutdown cannot look like a
            // complete capture, even when the observation queue itself is full.
            self.sender
                .budget
                .interrupted_streams
                .fetch_add(1, Ordering::Relaxed);
            self.offer_inner(Bytes::new(), Instant::now(), Some(StreamEnd::Interrupted));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            request_id: Uuid::new_v4(),
            direction: Direction::Response,
            transport: Transport::Http,
            content_type: "text/event-stream".into(),
            content_encoding: String::new(),
        }
    }

    #[test]
    fn observed_transports_are_counted_without_retaining_headers_or_payloads() {
        let (sender, _receiver) = channel(8, 8);
        for (direction, transport, content_type) in [
            (Direction::Request, Transport::Http, "application/json"),
            (
                Direction::Response,
                Transport::Http,
                "text/event-stream; charset=utf-8",
            ),
            (Direction::Response, Transport::Http, "application/json"),
            (Direction::Request, Transport::WebSocket, ""),
            (Direction::Response, Transport::WebSocket, ""),
        ] {
            let mut stream = sender.stream(Source {
                direction,
                transport,
                content_type: content_type.into(),
                ..source()
            });
            stream.finish();
        }
        let transports = sender.stats().transports;
        assert_eq!(transports.http_requests, 1);
        assert_eq!(transports.http_responses, 2);
        assert_eq!(transports.sse_responses, 1);
        assert_eq!(transports.websocket_requests, 1);
        assert_eq!(transports.websocket_responses, 1);
    }

    #[tokio::test]
    async fn full_byte_budget_reports_gaps_and_releases_bytes_after_consumption() {
        let (sender, mut receiver) = channel(8, 8);
        let mut stream = sender.stream(source());
        stream.offer(Bytes::from_static(b"12345678"), Instant::now());
        stream.offer(Bytes::from_static(b"lost"), Instant::now());
        assert_eq!(sender.stats().retained_bytes, 8);
        assert_eq!(sender.stats().dropped_chunks, 1);
        assert_eq!(sender.stats().dropped_bytes, 4);
        let first = receiver.recv().await.unwrap();
        assert_eq!(first.sequence, 1);
        // A consumer still processing a chunk remains charged to the budget.
        assert_eq!(sender.stats().retained_bytes, 8);
        drop(first);
        assert_eq!(sender.stats().retained_bytes, 0);
        stream.offer(Bytes::from_static(b"ok"), Instant::now());
        let next = receiver.recv().await.unwrap();
        assert_eq!(next.sequence, 3);
        assert_eq!(next.bytes, "ok");
    }

    #[tokio::test]
    async fn message_limit_bounds_zero_length_events_and_closed_consumer_never_blocks() {
        let (sender, mut receiver) = channel(8, 1);
        let mut stream = sender.stream(source());
        stream.finish();
        stream.finish();
        let mut second = sender.stream(source());
        second.finish();
        assert_eq!(sender.stats().dropped_chunks, 1);
        assert_eq!(
            receiver.recv().await.unwrap().end,
            Some(StreamEnd::Complete)
        );
        drop(receiver);
        let mut third = sender.stream(source());
        third.offer(Bytes::from_static(b"ignored"), Instant::now());
        assert_eq!(sender.stats().dropped_chunks, 2);
        assert_eq!(sender.stats().retained_bytes, 0);
    }

    #[tokio::test]
    async fn aborted_stream_is_explicit_even_when_its_terminal_observation_is_lost() {
        let (sender, mut receiver) = channel(8, 1);
        let mut stream = sender.stream(source());
        stream.offer(Bytes::from_static(b"first"), Instant::now());
        drop(stream);
        assert_eq!(sender.stats().interrupted_streams, 1);
        assert_eq!(sender.stats().dropped_chunks, 1);
        drop(receiver.recv().await.unwrap());
        let stream = sender.stream(source());
        drop(stream);
        let end = receiver.recv().await.unwrap();
        assert_eq!(end.sequence, 1);
        assert_eq!(end.end, Some(StreamEnd::Interrupted));
        assert_eq!(sender.stats().interrupted_streams, 2);
    }
}
