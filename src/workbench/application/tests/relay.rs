//! End-to-end trusted relay tests with isolated accounts, Application and synthetic CLI.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

struct Portal {
    _dir: tempfile::TempDir,
    store: codex_relayd::Store,
    origin: String,
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}
impl Portal {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let router = codex_relayd::app(codex_relayd::Config {
            data_dir: dir.path().join("relay"),
            portal_origin: origin.clone(),
            workbench_domain: "localhost".into(),
            workbench_origin: None,
            fixed_gateway_id: None,
            password_workers: 4,
            allow_insecure_loopback: true,
            trusted_client_ip_header: None,
        })
        .unwrap();
        let store = codex_relayd::Store::open(&dir.path().join("relay")).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            _dir: dir,
            store,
            origin,
            address,
            task,
        }
    }
    async fn account(&self, name: &str) -> String {
        let invite = self.store.issue(name, false).unwrap();
        let response = client()
            .post(format!("{}/relay/v1/register", self.origin))
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json")
            .body(
                json!({"token":invite,"password":"synthetic long unique phrase 8362"}).to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let response = self
            .request("", "/relay/v1/login")
            .body(
                json!({"username":name,"password":"synthetic long unique phrase 8362"}).to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .into()
    }
    fn request(&self, cookie: &str, path: &str) -> reqwest::RequestBuilder {
        client()
            .post(format!("{}{path}", self.origin))
            .header("Cookie", cookie)
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json")
    }
    async fn bind(&self, app: &Application, cookie: &str, name: &str) -> Remote {
        let service = app.web.relay_for_test();
        service
            .bind(self.origin.clone(), name.into(), true)
            .await
            .unwrap();
        let pending = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = service.status().await;
                if snapshot.binding.is_some() {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("binding code");
        let response = self
            .request(cookie, "/relay/v1/bindings/approve")
            .body(json!({"userCode":pending.binding.unwrap().user_code}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let status = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let snapshot = service.status().await;
                if snapshot.gateway_id.is_some() && !snapshot.enabled {
                    service.enable().await.unwrap();
                }
                if snapshot.state == "online" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("connector binding/handshake");
        let gateway = status.gateway_id.unwrap();
        let response = self
            .request(cookie, &format!("/relay/v1/gateways/{gateway}/open"))
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let url = reqwest::Url::parse(json_body(response).await["url"].as_str().unwrap()).unwrap();
        let code = url.fragment().unwrap().strip_prefix("code=").unwrap();
        let origin = status.gateway_origin.unwrap();
        let remote_client = reqwest::Client::builder()
            .no_proxy()
            .resolve(url.host_str().unwrap(), self.address)
            .build()
            .unwrap();
        let response = remote_client
            .post(format!("{origin}/relay/v1/session/exchange"))
            .header("Origin", &origin)
            .header("Content-Type", "application/json")
            .body(json!({"code":code}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let session_cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .into();
        Remote {
            client: remote_client,
            origin,
            cookie: session_cookie,
            gateway,
            address: self.address,
        }
    }
}

#[test]
fn styled_maximum_native_checkpoint_and_full_replay_fit_remote_snapshot_limit() {
    let mut journal = crate::workbench::terminal_screen::TerminalJournal::new(
        200,
        500,
        1024 * 1024,
        Duration::from_secs(60),
    );
    let mut output = Vec::new();
    for row in 1..=200 {
        output.extend_from_slice(format!("\x1b[{row};1H").as_bytes());
        for col in 0..500 {
            output.extend_from_slice(if col % 2 == 0 {
                b"\x1b[38;2;255;254;253;48;2;252;251;250;1;3;4;7m"
            } else {
                b"\x1b[38;2;250;251;252;48;2;253;254;255;22;23;24;27m"
            });
            output.extend_from_slice(
                "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}"
                    .as_bytes(),
            );
        }
    }
    journal.append(output);
    journal.append(vec![b' '; 1024 * 1024]);
    let snapshot = journal.snapshot(None);
    assert_eq!(snapshot.replay.len(), 1024 * 1024);
    let encoded = json!({"type":"snapshot","screen":STANDARD.encode(snapshot.screen),"replay":STANDARD.encode(snapshot.replay)}).to_string();
    assert!(encoded.len() > 2 * 1024 * 1024);
    assert!(encoded.len() + 4096 < codex_relay_protocol::MAX_WS_MESSAGE);
}
impl Drop for Portal {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct Remote {
    client: reqwest::Client,
    origin: String,
    cookie: String,
    gateway: Uuid,
    address: std::net::SocketAddr,
}
impl Remote {
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header("Cookie", &self.cookie)
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json")
    }
    async fn get(&self, path: &str) -> reqwest::Response {
        self.request(Method::GET, path).send().await.unwrap()
    }
    async fn terminal(&self, run: Uuid) -> WebSocketStream<tokio::net::TcpStream> {
        let mut request = format!(
            "{}/workbench/v1/runs/{run}/terminal?epoch={run}",
            self.origin.replace("http://", "ws://")
        )
        .into_client_request()
        .unwrap();
        request
            .headers_mut()
            .insert("origin", self.origin.parse().unwrap());
        request
            .headers_mut()
            .insert("cookie", self.cookie.parse().unwrap());
        tokio_tungstenite::client_async(
            request,
            tokio::net::TcpStream::connect(self.address).await.unwrap(),
        )
        .await
        .unwrap()
        .0
    }
}
async fn terminal_frame(socket: &mut WebSocketStream<tokio::net::TcpStream>, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == kind {
                        break value;
                    }
                }
                Message::Ping(value) => socket.send(Message::Pong(value)).await.unwrap(),
                Message::Binary(_) | Message::Pong(_) => {}
                other => panic!("unexpected terminal frame: {other:?}"),
            }
        }
    })
    .await
    .expect("remote terminal frame")
}

#[tokio::test]
async fn public_relay_routes_two_accounts_full_owner_http_terminal_and_logout_without_stopping_cli()
{
    let portal = Portal::start().await;
    let alice = portal.account("alice").await;
    let bob = portal.account("bob").await;
    let fixture_a = Fixture::new();
    let fixture_b = Fixture::new();
    let binary_a = fixture_a.cli();
    let binary_b = fixture_b.cli();
    std::fs::write(
        fixture_a.cwd.join("example.txt"),
        "synthetic workspace content",
    )
    .unwrap();
    let app_a = fixture_a.app(Overrides {
        codex_bin: Some(binary_a),
        ..Default::default()
    });
    let app_b = fixture_b.app(Overrides {
        codex_bin: Some(binary_b),
        ..Default::default()
    });
    let local_cookie = pair(&app_a).await;
    let (remote_a, remote_b) = tokio::join!(
        portal.bind(&app_a, &alice, "Alice computer"),
        portal.bind(&app_b, &bob, "Bob computer")
    );
    for (remote, app) in [(&remote_a, &app_a), (&remote_b, &app_b)] {
        let body = json_body(remote.get("/workbench/v1/application").await).await;
        assert_eq!(body["instanceId"], app.instance_id.to_string());
        assert_eq!(body["remoteOwner"], true);
        assert_eq!(body["directoryPickerAvailable"], false);
        assert_eq!(remote.get("/").await.status(), 200);
        for path in [
            "/workbench/v1/library/sources",
            "/workbench/v1/library/projects",
            "/workbench/v1/library/entries",
        ] {
            assert_eq!(remote.get(path).await.status(), 200, "{path}");
        }
        for path in [
            "/workbench/v1/application/connect",
            "/workbench/v1/application/relay",
            "/v1/health",
            "/v2/send",
            "/v1/responses",
        ] {
            assert_eq!(remote.get(path).await.status(), 404, "{path}");
        }
    }
    assert_eq!(
        portal
            .request(
                &bob,
                &format!("/relay/v1/gateways/{}/open", remote_a.gateway)
            )
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        remote_a
            .client
            .get(format!("{}/workbench/v1/application", remote_a.origin))
            .header("Cookie", &remote_b.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        remote_a
            .request(Method::POST, "/workbench/v1/launch-targets")
            .header("Origin", "https://evil.invalid")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    // No business streams remain; idle browser grants still need 10-second renewals.
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert_eq!(
        remote_a.get("/workbench/v1/application").await.status(),
        200
    );
    assert_eq!(
        remote_b.get("/workbench/v1/application").await.status(),
        200
    );
    let settings = json_body(remote_a.get("/workbench/v1/application/settings").await).await;
    let mut config: Config = serde_json::from_value(settings["settings"]["saved"].clone()).unwrap();
    config.launch.open_browser = false;
    let save = |instance: Uuid| {
        remote_a
            .request(Method::PUT, "/workbench/v1/application/settings")
            .header(
                "If-Match",
                format!("\"{}\"", settings["settings"]["revision"].as_str().unwrap()),
            )
            .body(json!({"instanceId":instance,"config":config}).to_string())
    };
    assert_eq!(save(Uuid::new_v4()).send().await.unwrap().status(), 409);
    assert_eq!(save(app_a.instance_id).send().await.unwrap().status(), 200);
    assert_eq!(save(app_a.instance_id).send().await.unwrap().status(), 412);
    let response = remote_a
        .request(Method::POST, "/workbench/v1/launch-targets")
        .body(json!({"path":fixture_a.cwd}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let target = json_body(response).await;
    let operation = Uuid::new_v4();
    let start = json!({"instanceId":app_a.instance_id,"targetId":target["targetId"],"mode":"new","operationId":operation,"configRevision":target["configRevision"]});
    assert_eq!(
        remote_a
            .request(Method::POST, "/workbench/v1/runs")
            .body(start.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let run = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let body = json_body(
                remote_a
                    .get(&format!("/workbench/v1/launch-operations/{operation}"))
                    .await,
            )
            .await;
            if body["state"] != "starting" {
                assert_eq!(body["state"], "ready", "{body}");
                break body["run"].clone();
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let run_id = Uuid::parse_str(run["runId"].as_str().unwrap()).unwrap();
    let prefix = format!("/workbench/v1/runs/{run_id}");
    assert_eq!(
        get(&app_a, &local_cookie, &format!("{prefix}/run"))
            .await
            .status(),
        200
    );
    for tail in [
        "run",
        "live/snapshot",
        "settings",
        "access",
        "history",
        "workspace/files",
        "workspace/file?path=example.txt",
    ] {
        assert_eq!(
            remote_a.get(&format!("{prefix}/{tail}")).await.status(),
            200,
            "{tail}"
        );
    }
    let response = remote_a
        .get(&format!("{prefix}/live/events?epoch={run_id}&after=0"))
        .await;
    assert_eq!(response.status(), 200);
    let mut events = response.bytes_stream();
    let mut terminal = remote_a.terminal(run_id).await;
    assert_eq!(
        terminal_frame(&mut terminal, "snapshot").await["runEpoch"],
        run_id.to_string()
    );
    terminal
        .send(Message::Text(
            json!({"runEpoch":run_id,"id":1,"command":{"type":"claim"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let grant = terminal_frame(&mut terminal, "grant").await;
    terminal.send(Message::Text(json!({"runEpoch":run_id,"id":2,"command":{"type":"input","generation":grant["grant"]["generation"],"sequence":1,"data":STANDARD.encode("synthetic remote input\n")}}).to_string().into())).await.unwrap();
    assert_eq!(terminal_frame(&mut terminal, "ack").await["id"], 2);
    if std::env::var_os("CODEX_RELAY_CHROME_TEST").is_some() {
        use tokio::io::AsyncWriteExt;
        let input = json!({"portalOrigin":portal.origin,"gateways":[
            {"origin":remote_a.origin,"cookie":remote_a.cookie,"accountCookie":alice,"expectedInstanceId":app_a.instance_id,"runId":run_id},
            {"origin":remote_b.origin,"cookie":remote_b.cookie,"accountCookie":bob,"expectedInstanceId":app_b.instance_id}
        ],"cwd":fixture_a.cwd});
        let mut child = tokio::process::Command::new("node")
            .arg("web/e2e/relay-public-probe.cjs")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .await
            .unwrap();
        let output = tokio::time::timeout(Duration::from_secs(90), child.wait_with_output())
            .await
            .expect("Chrome probe deadline")
            .unwrap();
        assert!(
            output.status.success(),
            "Chrome probe failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }
    assert_eq!(
        portal
            .request(&alice, "/relay/v1/logout")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(Ok(message)) = terminal.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .expect("logout closes terminal");
    tokio::time::timeout(Duration::from_secs(4), async {
        while events.next().await.is_some() {}
    })
    .await
    .expect("logout closes SSE");
    assert_eq!(
        remote_a.get("/workbench/v1/application").await.status(),
        401
    );
    assert_eq!(
        remote_b.get("/workbench/v1/application").await.status(),
        200
    );
    assert!(app_a.healthy());
    assert_eq!(
        get(&app_a, &local_cookie, &format!("{prefix}/run"))
            .await
            .status(),
        200
    );
    assert_eq!(
        unsafe { libc::kill(run["cliPid"].as_u64().unwrap() as i32, 0) },
        0
    );
    // Forget crosses the actual connector/server boundary and revokes credentials.
    let forgotten = app_b.web.relay_for_test().forget().await.unwrap();
    assert!(!forgotten.enabled);
    assert!(forgotten.gateway_id.is_none());
    assert!(forgotten.error.is_none());
    assert_eq!(
        remote_b.get("/workbench/v1/application").await.status(),
        401
    );
    let retained: i64 = portal
        .store
        .0
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM gateways WHERE id=?1",
            [remote_b.gateway.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, 0);
    assert!(app_b.healthy());
    portal.task.abort();
    assert!(app_a.healthy());
    assert!(
        client()
            .post(format!("{}{prefix}/stop", app_a.entry.address))
            .header("Cookie", &local_cookie)
            .header("Origin", &app_a.entry.address)
            .header("Content-Type", "application/json")
            .body(json!({"epoch":run_id}).to_string())
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
}
