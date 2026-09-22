use super::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::SinkExt;
use portable_pty::CommandBuilder;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::workbench::terminal::TerminalHost;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct Fixture {
    server: ReadingServer,
    _host: TerminalHost,
    directory: tempfile::TempDir,
    cookie: String,
}
impl Fixture {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let hub = LiveHub::new(LiveLimits::default());
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "stty -echo; printf 'READY\\r\\n'; while IFS= read -r line; do printf '%s\\n' \"$line\" >> inputs.txt; printf 'SEEN:%s\\r\\n' \"$line\"; done"]);
        command.cwd(directory.path());
        command.env("CODEX_HOME", directory.path());
        let host = TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
        let server = ReadingServer::bind_with_terminal(hub, host.handle())
            .await
            .unwrap();
        let cookie = cookie(&client(), &server).await;
        Self {
            server,
            _host: host,
            directory,
            cookie,
        }
    }
    fn request(&self) -> axum::http::Request<()> {
        let mut request = format!(
            "ws://{}/workbench/v1/terminal?epoch={}",
            self.server.address(),
            self.server.state.hub.epoch()
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert(header::ORIGIN, self.server.state.origin.parse().unwrap());
        request
            .headers_mut()
            .insert(header::COOKIE, self.cookie.parse().unwrap());
        request
    }
    async fn connect(&self) -> (Socket, Value) {
        let (mut socket, _) = connect_async(self.request()).await.unwrap();
        let hello = next_type(&mut socket, "snapshot").await;
        (socket, hello)
    }
    async fn command(&self, socket: &mut Socket, id: u64, command: Value, kind: &str) -> Value {
        socket
            .send(Message::Text(
                json!({"runEpoch":self.server.state.hub.epoch(),"id":id,"command":command})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        next_type(socket, kind).await
    }
    async fn input(
        &self,
        socket: &mut Socket,
        id: u64,
        generation: u64,
        sequence: u64,
        text: &str,
        kind: &str,
    ) -> Value {
        self.command(socket,id,json!({"type":"input","generation":generation,"sequence":sequence,"data":STANDARD.encode(text)}),kind).await
    }
    async fn wait_inputs(&self, expected: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(self.directory.path().join("inputs.txt"))
                    .ok()
                    .as_deref()
                    == Some(expected)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("synthetic PTY input not observed");
    }
}
async fn next_type(socket: &mut Socket, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    if frame["type"] == kind {
                        return frame;
                    }
                }
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                Message::Pong(_) => {}
                _ => panic!("terminal closed before expected frame"),
            }
        }
    })
    .await
    .expect("terminal frame deadline")
}
async fn denied(request: axum::http::Request<()>, status: StatusCode) {
    match connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), status)
        }
        _ => panic!("invalid terminal handshake must be denied"),
    }
}

#[tokio::test]
async fn terminal_handshake_requires_cookie_origin_host_and_current_epoch() {
    let fixture = Fixture::start().await;
    let mut request = fixture.request();
    request.headers_mut().remove(header::COOKIE);
    denied(request, StatusCode::UNAUTHORIZED).await;
    let mut request = fixture.request();
    request.headers_mut().remove(header::ORIGIN);
    denied(request, StatusCode::FORBIDDEN).await;
    let mut request = fixture.request();
    request
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.invalid".parse().unwrap());
    denied(request, StatusCode::FORBIDDEN).await;
    let mut request = fixture.request();
    request
        .headers_mut()
        .insert(header::HOST, "evil.invalid".parse().unwrap());
    denied(request, StatusCode::FORBIDDEN).await;
    let mut request = fixture.request();
    *request.uri_mut() = format!(
        "ws://{}/workbench/v1/terminal?epoch={}",
        fixture.server.address(),
        Uuid::new_v4()
    )
    .parse()
    .unwrap();
    denied(request, StatusCode::CONFLICT).await;
    let (_, hello) = fixture.connect().await;
    assert!(hello["control"]["controllerConnection"].is_null());
    assert!(!hello.to_string().contains("reconnectSecret"));
}

#[tokio::test]
async fn websocket_takeover_refresh_and_input_never_replay_or_accept_old_authority() {
    let fixture = Fixture::start().await;
    let (mut first, _) = fixture.connect().await;
    let (mut second, _) = fixture.connect().await;
    let rejected = fixture
        .input(&mut second, 1, 0, 1, "readonly\n", "error")
        .await;
    assert_eq!(rejected["error"]["code"]["control"], "stale_generation");
    let granted = fixture
        .command(&mut first, 1, json!({"type":"claim"}), "grant")
        .await;
    let old = granted["grant"]["generation"].as_u64().unwrap();
    fixture
        .input(&mut first, 2, old, 1, "中文\n第二行\n", "ack")
        .await;
    fixture.wait_inputs("中文\n第二行\n").await;
    let cancelled = fixture
        .command(
            &mut second,
            2,
            json!({"type":"takeover","confirmed":false}),
            "error",
        )
        .await;
    assert_eq!(cancelled["code"], "takeover_confirmation_required");
    let next = fixture
        .command(
            &mut second,
            3,
            json!({"type":"takeover","confirmed":true}),
            "grant",
        )
        .await;
    let generation = next["grant"]["generation"].as_u64().unwrap();
    let stale = fixture
        .input(&mut first, 3, old, 2, "stale\n", "error")
        .await;
    assert_eq!(stale["error"]["code"]["control"], "stale_generation");
    fixture
        .input(&mut second, 4, generation, 1, "new\n", "ack")
        .await;
    let duplicate = fixture
        .input(&mut second, 5, generation, 1, "duplicate\n", "error")
        .await;
    assert_eq!(duplicate["error"]["code"]["control"], "input_sequence");
    fixture.wait_inputs("中文\n第二行\nnew\n").await;
    let (mut refreshed, hello) = fixture.connect().await;
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(&STANDARD.decode(hello["screen"].as_str().unwrap()).unwrap());
    parser.process(&STANDARD.decode(hello["replay"].as_str().unwrap()).unwrap());
    assert!(parser.screen().contents().contains("SEEN:中文"));
    let resumed = fixture
        .command(
            &mut refreshed,
            1,
            json!({"type":"reconnect","secret":next["grant"]["reconnectSecret"]}),
            "grant",
        )
        .await;
    second.close(None).await.unwrap();
    let active = resumed["grant"]["generation"].as_u64().unwrap();
    fixture
        .input(&mut refreshed, 2, active, 1, "refresh\n", "ack")
        .await;
    fixture.wait_inputs("中文\n第二行\nnew\nrefresh\n").await;
    let resize = fixture
        .command(
            &mut refreshed,
            3,
            json!({"type":"resize","generation":active,"rows":30,"cols":100}),
            "ack",
        )
        .await;
    assert_eq!(resize["resized"], true);
    let resize = fixture
        .command(
            &mut refreshed,
            4,
            json!({"type":"resize","generation":active,"rows":30,"cols":100}),
            "ack",
        )
        .await;
    assert_eq!(resize["resized"], false);
    let (_, updated) = fixture.connect().await;
    assert_eq!(
        (updated["rows"].as_u64(), updated["cols"].as_u64()),
        (Some(30), Some(100))
    );
}

#[tokio::test]
async fn socket_identity_cannot_be_supplied_in_payload_and_old_epoch_cannot_write() {
    let fixture = Fixture::start().await;
    let (mut owner, hello) = fixture.connect().await;
    fixture
        .command(&mut owner, 1, json!({"type":"claim"}), "grant")
        .await;
    let (mut attacker, _) = fixture.connect().await;
    let rejected = fixture
        .command(
            &mut attacker,
            1,
            json!({"type":"release","generation":1,"connectionId":hello["connectionId"]}),
            "error",
        )
        .await;
    assert_eq!(rejected["code"], "invalid_frame");
    let (mut stale, _) = fixture.connect().await;
    stale
        .send(Message::Text(
            json!({"runEpoch":Uuid::new_v4(),"id":1,"command":{"type":"claim"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(next_type(&mut stale, "error").await["code"], "stale_run");
    assert!(!fixture.directory.path().join("inputs.txt").exists());
}

#[tokio::test]
async fn stop_checks_csrf_epoch_and_is_idempotent_without_respawn() {
    let fixture = Fixture::start().await;
    let url = format!("{}/workbench/v1/run/stop", fixture.server.state.origin);
    let client = client();
    let request = || {
        client
            .post(&url)
            .header(header::COOKIE, &fixture.cookie)
            .header(header::CONTENT_TYPE, "application/json")
    };
    let epoch = fixture.server.state.hub.epoch();
    let response = request()
        .body(json!({"epoch":epoch}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = request()
        .header(header::ORIGIN, &fixture.server.state.origin)
        .body(json!({"epoch":Uuid::new_v4()}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    for _ in 0..2 {
        let response = request()
            .header(header::ORIGIN, &fixture.server.state.origin)
            .body(json!({"epoch":epoch}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let (mut socket, hello) = fixture.connect().await;
    assert_eq!(hello["control"]["ended"], true);
    let rejected = fixture
        .command(&mut socket, 1, json!({"type":"claim"}), "error")
        .await;
    assert_eq!(rejected["error"]["code"]["control"], "ended");
}
