//! End-to-end trusted relay tests with isolated accounts, Application and synthetic CLI.
use super::*;
use crate::workbench::relay::channel_tests::{SecurePeer, SecureTerminal};
use base64::{Engine, engine::general_purpose::STANDARD};
use codex_relay_protocol::secure::{self, Scope};
use futures_util::StreamExt;
use reqwest::Method;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

struct Portal {
    _dir: tempfile::TempDir,
    store: codex_relayd::Store,
    origin: String,
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
    admin_cookie: String,
}
impl Portal {
    async fn start() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let store = codex_relayd::Store::open(&dir.path().join("relay")).unwrap();
        let password_file = dir.path().join("bootstrap-secret");
        std::fs::write(&password_file, "synthetic initial cloud phrase 1924").unwrap();
        std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        store.bootstrap_admin_from_file(&password_file).unwrap();
        let router = codex_relayd::app(codex_relayd::Config {
            data_dir: dir.path().join("relay"),
            portal_origin: origin.clone(),
            single_origin: true,
            password_workers: 4,
            allow_insecure_loopback: true,
            trusted_client_ip_header: None,
        })
        .unwrap();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let mut portal = Self {
            _dir: dir,
            store,
            origin,
            address,
            task,
            admin_cookie: String::new(),
        };
        let cookie = portal
            .login("windsyu", "synthetic initial cloud phrase 1924")
            .await;
        assert_eq!(portal.request(&cookie,"/relay/v1/password").body(json!({"oldPassword":"synthetic initial cloud phrase 1924","password":"synthetic managed forest phrase 5912"}).to_string()).send().await.unwrap().status(),200);
        portal.admin_cookie = portal
            .login("windsyu", "synthetic managed forest phrase 5912")
            .await;
        portal
    }
    async fn account(&self, name: &str) -> String {
        let response = self
            .request(&self.admin_cookie, "/relay/v1/admin/users")
            .body(
                json!({"username":name,"temporaryPassword":"synthetic temporary mountain 4826"})
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let cookie = self.login(name, "synthetic temporary mountain 4826").await;
        assert_eq!(self.request(&cookie,"/relay/v1/password").body(json!({"oldPassword":"synthetic temporary mountain 4826","password":"synthetic long unique phrase 8362"}).to_string()).send().await.unwrap().status(),200);
        self.login(name, "synthetic long unique phrase 8362").await
    }
    async fn login(&self, name: &str, password: &str) -> String {
        let response = self
            .request("", "/relay/v1/login")
            .body(json!({"username":name,"password":password}).to_string())
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
    fn assert_business_marker_absent_from_storage(&self) {
        let marker = b"E2EE_SYNTHETIC_SEARCH_MARKER";
        let directory = self._dir.path().join("relay");
        let mut checked = 0;
        for name in [
            "relay.sqlite3",
            "relay.sqlite3-wal",
            "relay.sqlite3-shm",
            "relay.sqlite3-journal",
        ] {
            let bytes = match std::fs::read(directory.join(name)) {
                Ok(bytes) => bytes,
                Err(error)
                    if name != "relay.sqlite3" && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(_) => panic!("could not inspect synthetic relay storage file: {name}"),
            };
            if name == "relay.sqlite3" {
                assert!(bytes.starts_with(b"SQLite format 3\0"));
            }
            assert!(
                !bytes.windows(marker.len()).any(|window| window == marker),
                "synthetic business content leaked into relay storage file: {name}"
            );
            checked += 1;
        }
        println!("Relay synthetic storage confidentiality: {checked} files checked");
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
                if snapshot.state == "online" {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .expect("connector binding/handshake");
        let gateway = status.gateway_id.unwrap();
        self.open_gateway(gateway, cookie).await
    }
    async fn open_gateway(&self, gateway: Uuid, cookie: &str) -> Remote {
        let response = self
            .request(cookie, &format!("/relay/v1/gateways/{gateway}/open"))
            .body(json!({"uiApiVersion":codex_relay_protocol::WORKBENCH_API_VERSION}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let url = reqwest::Url::parse(json_body(response).await["url"].as_str().unwrap()).unwrap();
        let grant = url
            .fragment()
            .unwrap()
            .split('&')
            .find_map(|pair| pair.strip_prefix("grant="))
            .unwrap()
            .to_owned();
        Remote {
            client: client(),
            origin: self.origin.clone(),
            cookie: cookie.to_owned(),
            gateway,
            address: self.address,
            prefix: format!("/g/{gateway}"),
            grant: Some(grant),
            url: url.to_string(),
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
    prefix: String,
    grant: Option<String>,
    url: String,
}
impl Remote {
    fn request(&self, method: Method, path: &str) -> RemoteRequest {
        self.request_as(method, path, &self.cookie, self.grant.as_deref())
    }
    fn request_as(
        &self,
        method: Method,
        path: &str,
        cookie: &str,
        grant: Option<&str>,
    ) -> RemoteRequest {
        let request = self
            .client
            .request(method, format!("{}{}{path}", self.origin, self.prefix))
            .header("Cookie", cookie)
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json");
        RemoteRequest {
            request,
            origin: self.origin.clone(),
            gateway: self.gateway,
            address: self.address,
            grant: grant.map(str::to_owned),
            path: path.into(),
        }
    }
    async fn get(&self, path: &str) -> reqwest::Response {
        self.request(Method::GET, path).send().await.unwrap()
    }
    async fn terminal(&self, run: Uuid) -> SecureTerminal {
        let request = self.request(
            Method::GET,
            &format!("/workbench/v1/runs/{run}/terminal?epoch={run}"),
        );
        let headers = request.request.build().unwrap().headers().clone();
        let peer = request_peer(
            &self.origin,
            self.gateway,
            self.address,
            self.grant.as_deref(),
            Scope::Run { run_id: run },
            &headers,
        )
        .await
        .unwrap()
        .unwrap();
        peer.terminal(1, &format!("/workbench/v1/runs/{run}/terminal?epoch={run}"))
            .await
            .unwrap()
    }
}
struct RemoteRequest {
    request: reqwest::RequestBuilder,
    origin: String,
    gateway: Uuid,
    address: std::net::SocketAddr,
    grant: Option<String>,
    path: String,
}
impl RemoteRequest {
    fn header<K, V>(mut self, name: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<axum::http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<axum::http::Error>,
    {
        self.request = self.request.header(name, value);
        self
    }
    fn body(mut self, value: impl Into<reqwest::Body>) -> Self {
        self.request = self.request.body(value);
        self
    }
    async fn send(self) -> anyhow::Result<reqwest::Response> {
        let request = self.request.build()?;
        let Some(scope) = secure::request_scope(request.method().as_str(), &self.path) else {
            return Ok(client().execute(request).await?);
        };
        let peer = request_peer(
            &self.origin,
            self.gateway,
            self.address,
            self.grant.as_deref(),
            scope,
            request.headers(),
        )
        .await?;
        let peer = match peer {
            Ok(peer) => peer,
            Err(status) => {
                return Ok(reqwest::Response::from(
                    axum::http::Response::builder()
                        .status(status)
                        .body(reqwest::Body::from(Vec::<u8>::new()))?,
                ));
            }
        };
        let headers = request
            .headers()
            .iter()
            .filter(|(name, _)| name.as_str() != "cookie")
            .map(|(name, value)| Ok((name.to_string(), value.to_str()?.to_owned())))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let method = request.method().as_str().to_owned();
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .unwrap_or_default();
        let mut peer = peer;
        peer.request(1, &method, &self.path, headers, body).await?;
        peer.response(1, &method).await
    }
}
async fn request_peer(
    origin: &str,
    gateway: Uuid,
    address: std::net::SocketAddr,
    grant: Option<&str>,
    scope: Scope,
    headers: &reqwest::header::HeaderMap,
) -> anyhow::Result<std::result::Result<SecurePeer, u16>> {
    let selector = match scope {
        Scope::Application => "scope=application".into(),
        Scope::Run { run_id } => format!("scope=run&runId={run_id}"),
    };
    let mut request = format!(
        "{}/g/{gateway}/workbench/v1/secure-channel?relayGrant={}&relayApi={}&{selector}",
        origin.replace("http://", "ws://"),
        grant.unwrap_or_default(),
        codex_relay_protocol::WORKBENCH_API_VERSION
    )
    .into_client_request()?;
    for name in ["cookie", "origin"] {
        for value in headers.get_all(name) {
            request.headers_mut().append(name, value.clone());
        }
    }
    let socket = match tokio_tungstenite::client_async(
        request,
        tokio::net::TcpStream::connect(address).await?,
    )
    .await
    {
        Ok((socket, _)) => socket,
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            return Ok(Err(response.status().as_u16()));
        }
        Err(error) => return Err(error.into()),
    };
    let (peer, binding) = SecurePeer::relay(socket).await?;
    assert_eq!(binding.context.gateway_id, gateway);
    assert_eq!(binding.context.grant_id.to_string(), grant.unwrap());
    assert_eq!(binding.scope, scope);
    Ok(Ok(peer))
}
async fn terminal_frame(socket: &mut SecureTerminal, kind: &str) -> Value {
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

async fn start_remote_run(remote: &Remote, app: &Application, cwd: &std::path::Path) -> Value {
    let saved = json_body(remote.get("/workbench/v1/application/settings").await).await;
    let mut config: Config = serde_json::from_value(saved["settings"]["saved"].clone()).unwrap();
    config.launch.open_browser = false;
    assert_eq!(
        remote
            .request(Method::PUT, "/workbench/v1/application/settings")
            .header(
                "If-Match",
                format!("\"{}\"", saved["settings"]["revision"].as_str().unwrap())
            )
            .body(json!({"instanceId":app.instance_id,"config":config}).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let response = remote
        .request(Method::POST, "/workbench/v1/launch-targets")
        .body(json!({"path":cwd}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let target = json_body(response).await;
    let operation = Uuid::new_v4();
    let response=remote.request(Method::POST,"/workbench/v1/runs").body(json!({"instanceId":app.instance_id,"targetId":target["targetId"],"mode":"new","operationId":operation,"configRevision":target["configRevision"]}).to_string()).send().await.unwrap();
    assert_eq!(response.status(), 202);
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let body = json_body(
                remote
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
    .unwrap()
}

async fn remote_library_organization(remote: &Remote, app: &Application, fixture: &Fixture) {
    let thread = Uuid::new_v4();
    std::fs::create_dir_all(fixture.home.join("sessions")).unwrap();
    let source = fixture.home.join("sessions/relay-organization.jsonl");
    let raw = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":thread,"source":"cli","cwd":fixture.cwd}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"Synthetic remote history conversation"}]}})
    );
    std::fs::write(&source, &raw).unwrap();
    assert_eq!(
        remote
            .request(Method::POST, "/workbench/v1/library/refresh")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let entry = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let response = remote
                .get("/workbench/v1/library/entries?kind=native")
                .await;
            assert_eq!(response.status(), 200);
            let body = json_body(response).await;
            if let Some(entry) = body["records"].as_array().and_then(|rows| {
                rows.iter()
                    .find(|row| row["nativeThreadId"] == thread.to_string())
            }) {
                break entry.clone();
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("synthetic native history indexed through relay");
    let response = remote
        .get("/workbench/v1/library/session-groups?kind=native")
        .await;
    assert_eq!(response.status(), 200);
    let groups = json_body(response).await;
    let group = groups["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["preferredEntryId"] == entry["entryId"])
        .expect("remote group includes indexed synthetic session");
    let mut detail = reqwest::Url::parse(&remote.origin).unwrap();
    detail.set_path(&format!(
        "/workbench/v1/library/session-groups/{}",
        group["groupId"].as_str().unwrap()
    ));
    detail
        .query_pairs_mut()
        .append_pair("anchorEntryId", entry["entryId"].as_str().unwrap())
        .append_pair("revision", groups["revision"].as_str().unwrap());
    let response = remote
        .get(&format!("{}?{}", detail.path(), detail.query().unwrap()))
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, *group);

    let path = format!(
        "/workbench/v1/library/entries/{}/organization",
        entry["entryId"].as_str().unwrap()
    );
    let response = remote.get(&path).await;
    assert_eq!(response.status(), 200);
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    let original = json_body(response).await;
    assert_eq!(original["canAdjust"], true);
    assert_eq!(
        etag,
        format!("\"{}\"", original["organizationRevision"].as_str().unwrap())
    );
    let body = json!({"instanceId":app.instance_id,"sourceIdentity":entry["sourceIdentity"],"sourceRevision":entry["sourceRevision"],"action":"unassigned"});
    for (header, value, status, code) in [
        (
            "If-Match",
            etag.as_str(),
            400,
            "invalid_organization_revision",
        ),
        ("Content-Type", "application/json", 415, "json_required"),
    ] {
        let rejected = remote
            .request(Method::PUT, &path)
            .header("If-Match", &etag)
            .header(header, value)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let actual_status = rejected.status();
        let rejected_body = rejected.text().await.unwrap();
        assert_eq!(actual_status, status, "duplicate {header}: {rejected_body}");
        // The opaque Relay cannot inspect encrypted preconditions. These exact
        // errors now come from the authenticated desktop API, before mutation.
        let rejected_body: Value = serde_json::from_str(&rejected_body).unwrap();
        assert_eq!(rejected_body["error"]["code"], code);
        let unchanged = remote.get(&path).await;
        assert_eq!(unchanged.status(), 200);
        assert_eq!(unchanged.headers()["etag"].to_str().unwrap(), etag);
        assert_eq!(json_body(unchanged).await, original);
    }
    let saved = remote
        .request(Method::PUT, &path)
        .header("If-Match", &etag)
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), 200);
    let saved_etag = saved.headers()["etag"].to_str().unwrap().to_owned();
    assert_ne!(saved_etag, etag);
    let saved = json_body(saved).await;
    assert_eq!(saved["organization"]["overrideState"], "applied");
    assert_eq!(
        saved["organization"]["effective"]["basis"],
        "user_unassigned"
    );
    assert!(saved["organization"]["effective"]["projectId"].is_null());
    let read = remote.get(&path).await;
    assert_eq!(read.status(), 200);
    assert_eq!(read.headers()["etag"].to_str().unwrap(), saved_etag);
    assert_eq!(json_body(read).await, saved);
    assert_eq!(std::fs::read_to_string(source).unwrap(), raw);
}

fn isolated_relay_cli(fixture: &Fixture) -> std::path::PathBuf {
    let cli = fixture.cli();
    std::fs::create_dir(fixture.dir.path().join("cli-user-home")).unwrap();
    let script = std::fs::read_to_string(&cli).unwrap();
    let script = script.replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nexport HOME=\"$CODEX_HOME/../cli-user-home\" USERPROFILE=\"$CODEX_HOME/../cli-user-home\"\n",
        1,
    );
    std::fs::write(&cli, script).unwrap();
    cli
}

async fn chrome_probe(script: &str, input: Value) {
    use tokio::io::AsyncWriteExt;
    let chrome_home = tempfile::tempdir().unwrap();
    let chrome_native_home = chrome_home.path().join(".codex");
    std::fs::create_dir(&chrome_native_home).unwrap();
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("HOME", chrome_home.path())
        .env("USERPROFILE", chrome_home.path())
        .env("CODEX_HOME", &chrome_native_home)
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

#[tokio::test]
async fn single_origin_three_computers_password_revocation_and_resume_preserve_ownership() {
    let portal = Portal::start().await;
    let alice = portal.account("alice").await;
    let bob = portal.account("bob").await;
    let first = Fixture::new();
    let second = Fixture::new();
    let third = Fixture::new();
    let app_a = first.app(Overrides {
        codex_bin: Some(isolated_relay_cli(&first)),
        ..Default::default()
    });
    let app_b = second.app(Overrides {
        codex_bin: Some(isolated_relay_cli(&second)),
        ..Default::default()
    });
    let app_c = third.app(Overrides {
        codex_bin: Some(isolated_relay_cli(&third)),
        ..Default::default()
    });
    let local_cookie = pair(&app_a).await;
    let (remote_a, remote_b, remote_c) = tokio::join!(
        portal.bind(&app_a, &alice, "Alice laptop"),
        portal.bind(&app_b, &alice, "Alice desktop"),
        portal.bind(&app_c, &bob, "Bob laptop")
    );
    remote_library_organization(&remote_a, &app_a, &first).await;
    assert_ne!(remote_a.gateway, remote_b.gateway);
    assert_ne!(remote_b.gateway, remote_c.gateway);
    for (remote, app) in [
        (&remote_a, &app_a),
        (&remote_b, &app_b),
        (&remote_c, &app_c),
    ] {
        assert_eq!(remote.origin, portal.origin);
        assert_eq!(
            json_body(remote.get("/workbench/v1/application").await).await["instanceId"],
            app.instance_id.to_string()
        );
        assert_eq!(remote.get("/").await.status(), 200);
    }
    let run = start_remote_run(&remote_a, &app_a, &first.cwd).await;
    let run_id = Uuid::parse_str(run["runId"].as_str().unwrap()).unwrap();
    let mut terminal = remote_a.terminal(run_id).await;
    assert_eq!(
        terminal_frame(&mut terminal, "snapshot").await["runEpoch"],
        run_id.to_string()
    );
    if std::env::var_os("CODEX_RELAY_CHROME_TEST").is_some() {
        // Dedicated portal parents let the browser revoke real server authority
        // without logging out the Rust fixture's independent active sessions.
        let probe_alice = portal
            .login("alice", "synthetic long unique phrase 8362")
            .await;
        let probe_bob = portal
            .login("bob", "synthetic long unique phrase 8362")
            .await;
        let (probe_a, probe_b, probe_c) = tokio::join!(
            portal.open_gateway(remote_a.gateway, &probe_alice),
            portal.open_gateway(remote_b.gateway, &probe_alice),
            portal.open_gateway(remote_c.gateway, &probe_bob)
        );
        chrome_probe("web/e2e/relay-single-origin-probe.cjs",json!({"portalOrigin":portal.origin,"gateways":[
            {"url":probe_a.url,"accountCookie":probe_alice,"expectedInstanceId":app_a.instance_id,"gatewayId":remote_a.gateway,"runId":run_id},
            {"url":probe_b.url,"accountCookie":probe_alice,"expectedInstanceId":app_b.instance_id,"gatewayId":remote_b.gateway},
            {"url":probe_c.url,"accountCookie":probe_bob,"expectedInstanceId":app_c.instance_id,"gatewayId":remote_c.gateway}
        ],"cwd":first.cwd})).await;
        portal.assert_business_marker_absent_from_storage();
    }
    let response = remote_a
        .request_as(
            Method::GET,
            "/workbench/v1/application",
            &bob,
            remote_a.grant.as_deref(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    let response = remote_a
        .request_as(
            Method::GET,
            "/workbench/v1/application",
            &alice,
            remote_b.grant.as_deref(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        portal
            .request(
                &portal.admin_cookie,
                &format!("/relay/v1/gateways/{}/open", remote_a.gateway)
            )
            .body(json!({"uiApiVersion":codex_relay_protocol::WORKBENCH_API_VERSION}).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let before: Vec<(String, String)> = {
        let c = portal.store.0.lock().unwrap();
        c.prepare("SELECT id,account FROM gateways ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    assert_eq!(portal.request(&alice,"/relay/v1/password").body(json!({"oldPassword":"synthetic long unique phrase 8362","password":"synthetic renewed glacier phrase 2856"}).to_string()).send().await.unwrap().status(),200);
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(Ok(message)) = terminal.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .expect("password change closes terminal");
    for remote in [&remote_a, &remote_b] {
        assert_eq!(remote.get("/workbench/v1/application").await.status(), 401);
    }
    assert_eq!(
        remote_c.get("/workbench/v1/application").await.status(),
        200
    );
    let alice_new = portal
        .login("alice", "synthetic renewed glacier phrase 2856")
        .await;
    assert_eq!(
        remote_a
            .request_as(
                Method::GET,
                "/workbench/v1/application",
                &alice_new,
                remote_a.grant.as_deref()
            )
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "old grant must not adopt a new portal session"
    );
    let entries = json_body(
        client()
            .get(format!("{}/relay/v1/gateways", portal.origin))
            .header("Cookie", &alice_new)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(entries["gateways"].as_array().unwrap().len(), 2);
    assert!(
        entries["gateways"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["reauthorizationRequired"] == true)
    );
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let a = app_a.web.relay_for_test().status().await;
            let b = app_b.web.relay_for_test().status().await;
            if a.state == "authenticationRequired" && b.state == "authenticationRequired" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("connector must stop and require explicit reauthentication");
    let (renewed_a, renewed_b) = tokio::join!(
        portal.bind(&app_a, &alice_new, "Alice laptop"),
        portal.bind(&app_b, &alice_new, "Alice desktop")
    );
    assert_eq!(renewed_a.gateway, remote_a.gateway);
    assert_eq!(renewed_b.gateway, remote_b.gateway);
    for (remote, app) in [
        (&renewed_a, &app_a),
        (&renewed_b, &app_b),
        (&remote_c, &app_c),
    ] {
        assert_eq!(
            json_body(remote.get("/workbench/v1/application").await).await["instanceId"],
            app.instance_id.to_string()
        );
        assert!(app.healthy());
    }
    let after: Vec<(String, String)> = {
        let c = portal.store.0.lock().unwrap();
        c.prepare("SELECT id,account FROM gateways ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    };
    assert_eq!(before, after);
    assert_eq!(
        unsafe { libc::kill(run["cliPid"].as_u64().unwrap() as i32, 0) },
        0,
        "account revocation must not stop local CLI"
    );
    assert_eq!(
        get(
            &app_a,
            &local_cookie,
            &format!("/workbench/v1/runs/{run_id}/run")
        )
        .await
        .status(),
        200
    );
    let stopped = renewed_a
        .request(Method::POST, &format!("/workbench/v1/runs/{run_id}/stop"))
        .body(json!({"epoch":run_id}).to_string())
        .send()
        .await
        .unwrap();
    let status = stopped.status();
    let stopped_body = stopped
        .text()
        .await
        .expect("remote stop must have authenticated ResponseEnd");
    assert!(
        status.is_success(),
        "remote stop failed: {status} {stopped_body}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&stopped_body).unwrap()["accepted"],
        true
    );
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let application = json_body(renewed_a.get("/workbench/v1/application").await).await;
            if application["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|run| run["runId"] == run_id.to_string() && run["state"] == "stopped")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("native CLI must actually exit before checking retained Run channels");
    for tail in ["run", "live/snapshot"] {
        let response = renewed_a
            .get(&format!("/workbench/v1/runs/{run_id}/{tail}"))
            .await;
        assert_eq!(
            response.status(),
            200,
            "stopped but retained Run must remain readable over a fresh Run channel"
        );
        assert!(
            response.bytes().await.is_ok(),
            "retained Run read must have authenticated End"
        );
    }
}

#[tokio::test]
async fn public_relay_routes_two_accounts_full_owner_http_terminal_and_logout_without_stopping_cli()
{
    let portal = Portal::start().await;
    let alice = portal.account("alice").await;
    let bob = portal.account("bob").await;
    let fixture_a = Fixture::new();
    let fixture_b = Fixture::new();
    let binary_a = isolated_relay_cli(&fixture_a);
    let binary_b = isolated_relay_cli(&fixture_b);
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
    remote_library_organization(&remote_a, &app_a, &fixture_a).await;
    for (remote, app, fixture) in [
        (&remote_a, &app_a, &fixture_a),
        (&remote_b, &app_b, &fixture_b),
    ] {
        let body = json_body(remote.get("/workbench/v1/application").await).await;
        assert_eq!(body["instanceId"], app.instance_id.to_string());
        assert_eq!(body["remoteOwner"], true);
        assert_eq!(body["directoryPickerAvailable"], false);
        assert_eq!(body["directoryBrowserAvailable"], true);
        let browse = remote
            .request(Method::POST, "/workbench/v1/application/directories")
            .body(json!({"instanceId":app.instance_id}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(browse.status(), 200);
        let browse = json_body(browse).await;
        assert_eq!(
            browse["homePath"],
            fixture.dir.path().canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(
            remote
                .request(Method::POST, "/workbench/v1/application/directories")
                .body(json!({"instanceId":Uuid::new_v4()}).to_string())
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        for path in [
            "/workbench/v1/application/control",
            "/workbench/v1/application/connect",
        ] {
            assert_eq!(
                remote
                    .request(Method::POST, path)
                    .body("{}")
                    .send()
                    .await
                    .unwrap()
                    .status(),
                404,
                "private lifecycle route must not enter relay allowlist"
            );
        }
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
            .body(json!({"uiApiVersion":codex_relay_protocol::WORKBENCH_API_VERSION}).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        remote_a
            .client
            .get(format!(
                "{}/relay/v1/gateways/{}/access",
                remote_a.origin, remote_a.gateway
            ))
            .header("Cookie", &remote_b.cookie)
            .header("x-workbench-grant", remote_a.grant.as_deref().unwrap())
            .header(
                "x-workbench-api-version",
                codex_relay_protocol::WORKBENCH_API_VERSION
            )
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        remote_a
            .request_as(
                Method::POST,
                "/workbench/v1/application/directories",
                &remote_b.cookie,
                remote_a.grant.as_deref()
            )
            .body(json!({"instanceId":app_a.instance_id}).to_string())
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
        let probe_alice = portal
            .login("alice", "synthetic long unique phrase 8362")
            .await;
        let probe_bob = portal
            .login("bob", "synthetic long unique phrase 8362")
            .await;
        let (probe_a, probe_b) = tokio::join!(
            portal.open_gateway(remote_a.gateway, &probe_alice),
            portal.open_gateway(remote_b.gateway, &probe_bob)
        );
        let input = json!({"portalOrigin":portal.origin,"gateways":[
            {"url":probe_a.url,"accountCookie":probe_alice,"expectedInstanceId":app_a.instance_id,"gatewayId":remote_a.gateway,"runId":run_id},
            {"url":probe_b.url,"accountCookie":probe_bob,"expectedInstanceId":app_b.instance_id,"gatewayId":remote_b.gateway}
        ],"cwd":fixture_a.cwd});
        chrome_probe("web/e2e/relay-single-origin-probe.cjs", input).await;
        portal.assert_business_marker_absent_from_storage();
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
    let stopped = client()
        .post(format!("{}{prefix}/stop", app_a.entry.address))
        .header("Cookie", &local_cookie)
        .header("Origin", &app_a.entry.address)
        .header("Content-Type", "application/json")
        .body(json!({"epoch":run_id}).to_string())
        .send()
        .await
        .unwrap();
    assert!(stopped.status().is_success());
    assert_eq!(json_body(stopped).await["accepted"], true);
}
