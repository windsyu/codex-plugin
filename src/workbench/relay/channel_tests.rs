//! Test-only phone endpoint. The real Relay forwards these records unchanged.
//! This is intentionally independent of the browser multiplexer implementation.
use super::crypto::{Cipher, Ephemeral};
use anyhow::{Result, bail, ensure};
use axum::extract::ws::{Message as ServerMessage, WebSocket};
use base64::{Engine, engine::general_purpose::STANDARD};
use codex_relay_protocol::secure::{self, ChannelBinding, Frame, Handshake, Kind};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{BTreeSet, VecDeque},
    io,
    time::Duration,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

enum Socket {
    Relay(WebSocketStream<TcpStream>),
    Mock(WebSocket),
}
impl Socket {
    async fn send(&mut self, message: Message) -> Result<()> {
        match self {
            Self::Relay(socket) => socket.send(message).await?,
            Self::Mock(socket) => {
                let message = match message {
                    Message::Text(text) => ServerMessage::Text(text.to_string().into()),
                    Message::Binary(bytes) => ServerMessage::Binary(bytes),
                    Message::Ping(bytes) => ServerMessage::Ping(bytes),
                    Message::Pong(bytes) => ServerMessage::Pong(bytes),
                    Message::Close(_) => ServerMessage::Close(None),
                    _ => bail!("test_socket_frame"),
                };
                socket.send(message).await?;
            }
        }
        Ok(())
    }
    async fn next(&mut self) -> Result<Message> {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let message = match self {
                    Self::Relay(socket) => socket.next().await.transpose()?,
                    Self::Mock(socket) => {
                        socket
                            .recv()
                            .await
                            .transpose()?
                            .map(|message| match message {
                                ServerMessage::Text(text) => Message::Text(text.to_string().into()),
                                ServerMessage::Binary(bytes) => Message::Binary(bytes),
                                ServerMessage::Ping(bytes) => Message::Ping(bytes),
                                ServerMessage::Pong(bytes) => Message::Pong(bytes),
                                ServerMessage::Close(_) => Message::Close(None),
                            })
                    }
                };
                match message {
                    Some(Message::Ping(bytes)) => self.send(Message::Pong(bytes)).await?,
                    Some(Message::Pong(_)) => {}
                    Some(Message::Close(_)) | None => bail!("test_channel_closed"),
                    Some(message) => return Ok(message),
                }
            }
        })
        .await?
    }
}

pub(crate) struct SecurePeer {
    socket: Socket,
    send: Cipher,
    receive: Cipher,
    closed: BTreeSet<u32>,
}
impl SecurePeer {
    pub(crate) async fn mock(socket: WebSocket, binding: &ChannelBinding) -> Result<Self> {
        Self::handshake(Socket::Mock(socket), binding).await
    }
    pub(crate) async fn relay(
        socket: WebSocketStream<TcpStream>,
    ) -> Result<(Self, ChannelBinding)> {
        let mut socket = Socket::Relay(socket);
        let Message::Text(text) = socket.next().await? else {
            bail!("test_binding_required");
        };
        let Handshake::ChannelBinding { binding } = serde_json::from_str(&text)? else {
            bail!("test_binding_required");
        };
        ensure!(binding.valid(), "test_binding_invalid");
        Ok((Self::handshake(socket, &binding).await?, binding))
    }
    async fn handshake(mut socket: Socket, binding: &ChannelBinding) -> Result<Self> {
        let key = Ephemeral::new()?;
        socket
            .send(Message::Text(
                serde_json::to_string(&Handshake::ClientHello {
                    public_key: key.public.clone(),
                })?
                .into(),
            ))
            .await?;
        let Message::Text(text) = socket.next().await? else {
            bail!("test_server_hello_required");
        };
        let Handshake::ServerHello { public_key } = serde_json::from_str(&text)? else {
            bail!("test_server_hello_required");
        };
        let (mut send, mut receive) = key.agree(binding, &public_key, false)?;
        socket
            .send(Message::Binary(send.seal(&Frame::Finished)?.into()))
            .await?;
        let Message::Binary(bytes) = socket.next().await? else {
            bail!("test_server_finished_required");
        };
        receive.finished(&bytes)?;
        Ok(Self {
            socket,
            send,
            receive,
            closed: BTreeSet::new(),
        })
    }
    pub(crate) async fn send(&mut self, frame: &Frame) -> Result<()> {
        let wire = self.send.seal(frame)?;
        self.socket.send(Message::Binary(wire.into())).await?;
        if let Frame::Reset { id, .. } = frame {
            self.closed.insert(*id);
        }
        Ok(())
    }
    pub(crate) async fn next(&mut self) -> Result<Frame> {
        loop {
            let Message::Binary(bytes) = self.socket.next().await? else {
                bail!("test_encrypted_record_required");
            };
            // Closed-child records remain authenticated channel records: their
            // global sequence must advance before their child is discarded.
            let frame = self.receive.open(&bytes)?;
            let id = match &frame {
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
            };
            if self.closed.contains(&id) {
                continue;
            }
            if matches!(frame, Frame::Reset { .. } | Frame::End { .. }) {
                self.closed.insert(id);
            }
            return Ok(frame);
        }
    }
    pub(crate) async fn close(&mut self) -> Result<()> {
        self.socket.send(Message::Close(None)).await
    }
    pub(crate) async fn accepted(&mut self, id: u32) -> Result<usize> {
        match self.next().await? {
            Frame::Accepted { id: actual, credit } if actual == id => Ok(credit),
            frame => bail!("test_accepted_required: {}", serde_json::to_string(&frame)?),
        }
    }
    pub(crate) async fn request(
        &mut self,
        id: u32,
        method: &str,
        path: &str,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Result<()> {
        self.send(&Frame::Open {
            id,
            kind: Kind::Http,
            method: method.into(),
            path: path.into(),
            headers,
            body_length: body.len(),
            receive_window: secure::WINDOW,
        })
        .await?;
        let mut credit = self.accepted(id).await?;
        for part in body.chunks(secure::CHUNK) {
            while credit < part.len() {
                match self.next().await? {
                    Frame::Window {
                        id: actual,
                        credit: more,
                    } if actual == id => credit += more,
                    _ => bail!("test_request_credit_required"),
                }
            }
            credit -= part.len();
            self.send(&Frame::Chunk {
                id,
                data: STANDARD.encode(part),
            })
            .await?;
        }
        self.send(&Frame::End { id }).await
    }
    pub(crate) async fn response(mut self, id: u32, method: &str) -> Result<reqwest::Response> {
        let (status, headers) = loop {
            match self.next().await? {
                Frame::Head {
                    id: actual,
                    status,
                    headers,
                } if actual == id => break (status, headers),
                Frame::Window { id: actual, .. } if actual == id => {}
                frame => bail!(
                    "test_response_head_required: {}",
                    serde_json::to_string(&frame)?
                ),
            }
        };
        let mut builder = axum::http::Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        if method == "HEAD" || matches!(status, 204 | 304) {
            ensure!(
                matches!(self.next().await?, Frame::End { id: actual } if actual == id),
                "test_response_end_required"
            );
            return Ok(reqwest::Response::from(
                builder.body(reqwest::Body::from(Vec::<u8>::new()))?,
            ));
        }
        let body: futures_util::stream::BoxStream<
            'static,
            std::result::Result<Vec<u8>, io::Error>,
        > = Box::pin(async_stream::try_stream! {
            loop {
                match self.next().await.map_err(io::Error::other)? {
                    Frame::Chunk { id: actual, data } if actual == id => {
                        let bytes = STANDARD.decode(data).map_err(io::Error::other)?;
                        self.send(&Frame::Window { id, credit: bytes.len() }).await.map_err(io::Error::other)?;
                        yield bytes;
                    }
                    Frame::End { id: actual } if actual == id => break,
                    _ => Err(io::Error::other("test_response_interrupted"))?,
                }
            }
        });
        Ok(reqwest::Response::from(
            builder.body(reqwest::Body::wrap_stream(body))?,
        ))
    }
    pub(crate) async fn terminal(mut self, id: u32, path: &str) -> Result<SecureTerminal> {
        self.send(&Frame::Open {
            id,
            kind: Kind::Terminal,
            method: "GET".into(),
            path: path.into(),
            headers: vec![],
            body_length: 0,
            receive_window: 0,
        })
        .await?;
        ensure!(
            self.accepted(id).await? == 0,
            "test_terminal_initial_credit"
        );
        self.send(&Frame::End { id }).await?;
        ensure!(
            matches!(self.next().await?, Frame::TerminalReady { id: actual } if actual == id),
            "test_terminal_ready_required"
        );
        Ok(SecureTerminal {
            peer: self,
            id,
            credit: 0,
            assembly: None,
            received: VecDeque::new(),
            closed: false,
        })
    }
}

struct Assembly {
    bytes: Vec<u8>,
    length: usize,
    credit: usize,
    binary: bool,
}
pub(crate) struct SecureTerminal {
    peer: SecurePeer,
    id: u32,
    credit: usize,
    assembly: Option<Assembly>,
    received: VecDeque<Message>,
    closed: bool,
}
impl SecureTerminal {
    async fn receive(&mut self) -> Result<()> {
        match self.peer.next().await? {
            Frame::Window { id, credit } if id == self.id => self.credit += credit,
            Frame::MessageStart { id, length, binary }
                if id == self.id && self.assembly.is_none() =>
            {
                ensure!(
                    length <= codex_relay_protocol::MAX_WS_MESSAGE,
                    "test_terminal_size"
                );
                let credit = length.min(secure::WINDOW);
                self.assembly = Some(Assembly {
                    bytes: Vec::with_capacity(length),
                    length,
                    credit,
                    binary,
                });
                if credit > 0 {
                    self.peer.send(&Frame::Window { id, credit }).await?;
                }
            }
            Frame::Chunk { id, data } if id == self.id => {
                let assembly = self
                    .assembly
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("test_message_start_required"))?;
                let bytes = STANDARD.decode(data)?;
                ensure!(
                    bytes.len() <= assembly.credit
                        && assembly.bytes.len() + bytes.len() <= assembly.length,
                    "test_terminal_window"
                );
                assembly.credit -= bytes.len();
                assembly.bytes.extend(bytes);
                let credit = (assembly.length - assembly.bytes.len() - assembly.credit)
                    .min(secure::WINDOW - assembly.credit);
                if credit > 0 {
                    assembly.credit += credit;
                    self.peer.send(&Frame::Window { id, credit }).await?;
                }
            }
            Frame::MessageEnd { id } if id == self.id => {
                let assembly = self
                    .assembly
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("test_message_start_required"))?;
                ensure!(
                    assembly.bytes.len() == assembly.length,
                    "test_terminal_truncated"
                );
                self.received.push_back(if assembly.binary {
                    Message::Binary(assembly.bytes.into())
                } else {
                    Message::Text(String::from_utf8(assembly.bytes)?.into())
                });
            }
            Frame::End { id } if id == self.id => {
                self.closed = true;
            }
            _ => bail!("test_terminal_interrupted"),
        }
        Ok(())
    }
    pub(crate) async fn send(&mut self, message: Message) -> Result<()> {
        let (bytes, binary) = match message {
            Message::Text(text) => (text.as_bytes().to_vec(), false),
            Message::Binary(bytes) => (bytes.to_vec(), true),
            Message::Close(_) => return self.peer.close().await,
            Message::Pong(_) => return Ok(()),
            _ => bail!("test_terminal_input"),
        };
        self.peer
            .send(&Frame::MessageStart {
                id: self.id,
                length: bytes.len(),
                binary,
            })
            .await?;
        for part in bytes.chunks(secure::CHUNK) {
            while self.credit < part.len() {
                self.receive().await?;
            }
            self.credit -= part.len();
            self.peer
                .send(&Frame::Chunk {
                    id: self.id,
                    data: STANDARD.encode(part),
                })
                .await?;
        }
        self.peer.send(&Frame::MessageEnd { id: self.id }).await
    }
    pub(crate) async fn next(&mut self) -> Option<Result<Message>> {
        loop {
            if let Some(message) = self.received.pop_front() {
                return Some(Ok(message));
            }
            if self.closed {
                return None;
            }
            if let Err(error) = self.receive().await {
                self.closed = true;
                return Some(Err(error));
            }
        }
    }
}
