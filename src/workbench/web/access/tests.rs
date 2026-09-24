use super::*;
use crate::workbench::live::LiveLimits;
use crate::workbench::terminal::TerminalHost;
use futures_util::SinkExt;
use portable_pty::CommandBuilder;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

struct Fixture {
    server: ReadingServer,
    _host: TerminalHost,
    _dir: tempfile::TempDir,
    client: reqwest::Client,
    owner: String,
    _config: Option<crate::workbench::config::ConfigService>,
}
impl Fixture {
    async fn new() -> Self {
        Self::with_config(None).await
    }
    async fn with_config(config: Option<crate::workbench::config::Config>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let hub = LiveHub::new(LiveLimits::default());
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "stty -echo; while IFS= read -r line; do printf '%s\\n' \"$line\" >> inputs; done",
        ]);
        command.cwd(dir.path());
        for key in ["HOME", "USERPROFILE", "CODEX_HOME"] {
            command.env(key, dir.path());
        }
        let host = TerminalHost::spawn(hub.epoch(), command, 24, 80).unwrap();
        let config = config.map(|config| {
            use crate::workbench::config::{ConfigService, Overrides, Prepared};
            let paths =
                crate::workbench::paths::WorkbenchPaths::from_user_home(dir.path()).unwrap();
            let native_home = dir.path().join("native-home");
            std::fs::create_dir(&native_home).unwrap();
            let prepared =
                Prepared::load(&native_home, dir.path(), &paths, None, Overrides::default())
                    .unwrap();
            std::fs::write(
                paths.config_dir().join("config.json"),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
            ConfigService::start(prepared).unwrap()
        });
        let server = ReadingServer::bind_options(
            hub,
            RuntimeOptions {
                terminal: Some(host.handle()),
                settings: config.as_ref().map(|c| c.handle()),
                ..RuntimeOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        *server.state.access.discovery.lock().unwrap() = Some(discovery::Found {
            hosts: vec![
                ("127.0.0.1".into(), "局域网".into()),
                ("machine.tail-test.ts.net".into(), "Tailscale".into()),
            ],
            notice: None,
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let response = client
            .post(format!("{}/workbench/v1/pair", server.state.origin))
            .header(header::ORIGIN, &server.state.origin)
            .body(json!({"token":server.state.pairing}).to_string())
            .send()
            .await
            .unwrap();
        let owner = cookie(&response);
        Self {
            server,
            _host: host,
            _dir: dir,
            client,
            owner,
            _config: config,
        }
    }
    async fn status(&self) -> Value {
        json_response(
            self.client
                .get(format!("{}/workbench/v1/access", self.server.state.origin))
                .header(header::COOKIE, &self.owner)
                .send()
                .await
                .unwrap(),
        )
        .await
    }
    async fn action(&self, path: &str) -> reqwest::Response {
        let current = self.status().await;
        self.client
            .post(format!(
                "{}/workbench/v1/access/{path}",
                self.server.state.origin
            ))
            .header(header::COOKIE, &self.owner)
            .header(header::ORIGIN, &self.server.state.origin)
            .header(header::IF_MATCH, format!("\"{}\"", current["revision"]))
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"runEpoch":self.server.state.hub.epoch()}).to_string())
            .send()
            .await
            .unwrap()
    }
    async fn ready(&self) -> Value {
        assert_eq!(self.action("enable").await.status(), StatusCode::ACCEPTED);
        for _ in 0..100 {
            let value = self.status().await;
            if value["state"] == "ready" {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("listener did not become ready")
    }
    async fn pairing(&self) -> Value {
        let r = self
            .client
            .get(format!(
                "{}/workbench/v1/access/pairing",
                self.server.state.origin
            ))
            .header(header::COOKIE, &self.owner)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        json_response(r).await
    }
    async fn pair(&self, base: &str, origin: &str, token: &str) -> reqwest::Response {
        self.client
            .post(format!("{base}/workbench/v1/pair"))
            .header(header::HOST, origin.strip_prefix("http://").unwrap())
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"token":token}).to_string())
            .send()
            .await
            .unwrap()
    }
}
fn token(invite: &Value) -> &str {
    invite["links"][0]["url"]
        .as_str()
        .unwrap()
        .split("#pair=")
        .nth(1)
        .unwrap()
}
fn cookie(response: &reqwest::Response) -> String {
    response.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}

#[tokio::test]
async fn two_addresses_share_reusable_code_runtime_and_admin_boundaries() {
    let f = Fixture::new().await;
    assert_eq!(f.status().await["state"], "off");
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let dns = ready["addresses"][1]["origin"].as_str().unwrap();
    let invite = f.pairing().await;
    assert_eq!(
        token(&invite),
        invite["links"][1]["url"]
            .as_str()
            .unwrap()
            .split("#pair=")
            .nth(1)
            .unwrap()
    );
    let (first, second) = tokio::join!(
        f.pair(ip, ip, token(&invite)),
        f.pair(ip, dns, token(&invite))
    );
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert_eq!(second.status(), StatusCode::NO_CONTENT);
    let device = cookie(&first);
    assert_ne!(device, cookie(&second));
    assert_eq!(f.status().await["devices"].as_array().unwrap().len(), 2);
    for origin in [ip, dns] {
        let result = f
            .client
            .get(format!("{ip}/workbench/v1/run"))
            .header(header::HOST, origin.strip_prefix("http://").unwrap())
            .header(header::COOKIE, &device)
            .send()
            .await
            .unwrap();
        let result: Value = json_response(result).await;
        assert_eq!(result["runEpoch"], f.server.state.hub.epoch().to_string());
        assert_eq!(result["processId"], f._host.process_id());
        assert_eq!(result["accessAvailable"], false);
    }
    let denied = f
        .client
        .get(format!("{ip}/workbench/v1/access/pairing"))
        .header(header::COOKIE, &device)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    for (name, value) in [
        (header::HOST, "unknown.invalid"),
        (header::ORIGIN, "null"),
        (header::ORIGIN, "https://untrusted.invalid"),
    ] {
        assert_eq!(
            f.client
                .get(format!("{ip}/workbench/v1/run"))
                .header(header::COOKIE, &device)
                .header(name, value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        f.client
            .post(format!("{ip}/workbench/v1/pair"))
            .body(json!({"token":token(&invite)}).to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let spoof = f
        .client
        .get(format!("{ip}/workbench/v1/run"))
        .header(header::HOST, &f.server.state.authority)
        .header(header::COOKIE, &f.owner)
        .send()
        .await
        .unwrap();
    assert_eq!(spoof.status(), StatusCode::FORBIDDEN);
    let cross = f
        .client
        .get(format!("{ip}/workbench/v1/run"))
        .header(header::ORIGIN, dns)
        .header(header::COOKIE, &device)
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), StatusCode::FORBIDDEN);
    let missing = f
        .client
        .get(format!("{ip}/workbench/v1/live/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        f.pair(ip, ip, &f.server.state.pairing).await.status(),
        StatusCode::GONE
    );
    let repeated = f.pairing().await;
    assert_eq!(token(&invite), token(&repeated));
    assert_eq!(
        repeated,
        f.pairing().await,
        "reading the QR neither rotates the code nor changes revision"
    );
    assert!(
        !f.status().await.to_string().contains(token(&invite)),
        "status must not expose the code"
    );
    assert_eq!(f.action("disable").await.status(), StatusCode::ACCEPTED);
    for _ in 0..50 {
        if f.status().await["state"] == "off" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.status().await["state"], "off");
    assert_eq!(
        pair_device(&f.server.state, token(&invite), &HeaderMap::new()).status(),
        StatusCode::GONE
    );
    let reopened = f.ready().await;
    assert_eq!(token(&f.pairing().await), token(&invite));
    let next = reopened["addresses"][0]["origin"].as_str().unwrap();
    assert_eq!(
        f.client
            .get(format!("{next}/workbench/v1/run"))
            .header(header::COOKIE, &device)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.pair(next, next, token(&invite)).await.status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn revocation_closes_ws_sse_reconnect_and_preserves_other_browsers() {
    let f = Fixture::new().await;
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let a = f.pairing().await;
    let ca = cookie(&f.pair(ip, ip, token(&a)).await);
    let b = f.pairing().await;
    let cb = cookie(&f.pair(ip, ip, token(&b)).await);
    let mut req = format!(
        "{}/workbench/v1/terminal?epoch={}",
        ip.replace("http:", "ws:"),
        f.server.state.hub.epoch()
    )
    .into_client_request()
    .unwrap();
    req.headers_mut()
        .insert(header::ORIGIN, ip.parse().unwrap());
    req.headers_mut()
        .insert(header::COOKIE, ca.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    loop {
        let item = ws.next().await.unwrap().unwrap();
        if item.is_text() {
            break;
        }
    }
    ws.send(Message::Text(
        json!({"runEpoch":f.server.state.hub.epoch(),"id":1,"command":{"type":"claim"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let grant = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let msg = ws.next().await.unwrap().unwrap();
            if let Ok(v) = serde_json::from_str::<Value>(msg.to_text().unwrap_or(""))
                && v["type"] == "grant"
            {
                break v;
            }
        }
    })
    .await
    .unwrap();
    let mut events = f
        .client
        .get(format!(
            "{ip}/workbench/v1/live/events?epoch={}&after=0",
            f.server.state.hub.epoch()
        ))
        .header(header::COOKIE, &ca)
        .send()
        .await
        .unwrap()
        .bytes_stream();
    let current = f.status().await;
    let id = current["devices"][0]["id"].as_str().unwrap();
    let response = f
        .client
        .delete(format!(
            "{}/workbench/v1/access/devices/{id}?runEpoch={}",
            f.server.state.origin,
            f.server.state.hub.epoch()
        ))
        .header(header::COOKIE, &f.owner)
        .header(header::ORIGIN, &f.server.state.origin)
        .header(header::IF_MATCH, format!("\"{}\"", current["revision"]))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(_)) = events.next().await {}
    })
    .await
    .unwrap();
    assert_eq!(
        f.client
            .get(format!("{ip}/workbench/v1/run"))
            .header(header::COOKIE, &ca)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.client
            .get(format!("{ip}/workbench/v1/run"))
            .header(header::COOKIE, &cb)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let local = f._host.handle().attach().await.unwrap();
    assert!(!local.control.reconnect_reserved);
    assert!(
        f._host
            .handle()
            .reconnect(
                local.connection_id,
                grant["grant"]["reconnectSecret"].as_str().unwrap().into()
            )
            .await
            .is_err()
    );
    assert!(f._host.handle().claim(local.connection_id).await.is_ok());
    // Revocation ends this session, while the shared run code can pair again.
    let rejoined = f.pair(ip, ip, token(&a)).await;
    assert_eq!(rejoined.status(), StatusCode::NO_CONTENT);
    assert_ne!(cookie(&rejoined), ca);
}

#[tokio::test]
async fn stale_revisions_limits_and_current_generation_are_enforced() {
    let f = Fixture::new().await;
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let response = f
        .client
        .post(format!(
            "{}/workbench/v1/access/disable",
            f.server.state.origin
        ))
        .header(header::COOKIE, &f.owner)
        .header(header::ORIGIN, &f.server.state.origin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"runEpoch":f.server.state.hub.epoch()}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_REQUIRED);
    let response = f
        .client
        .post(format!(
            "{}/workbench/v1/access/disable",
            f.server.state.origin
        ))
        .header(header::COOKIE, &f.owner)
        .header(header::ORIGIN, &f.server.state.origin)
        .header(header::IF_MATCH, "\"0\"")
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"runEpoch":f.server.state.hub.epoch()}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
    let mut first_cookie = None;
    for _ in 0..MAX_DEVICES {
        let invite = f.pairing().await;
        let response = f.pair(ip, ip, token(&invite)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        first_cookie.get_or_insert_with(|| cookie(&response));
    }
    let invite = f.pairing().await;
    assert_eq!(
        f.pair(ip, ip, token(&invite)).await.status(),
        StatusCode::CONFLICT
    );
    let before = f.status().await;
    for value in ["invalid".to_owned(), token(&invite).to_owned()] {
        let repeated = f
            .client
            .post(format!("{ip}/workbench/v1/pair"))
            .header(header::ORIGIN, ip)
            .header(header::COOKIE, first_cookie.as_ref().unwrap())
            .body(json!({"token":value}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            repeated.status(),
            if value == "invalid" {
                StatusCode::GONE
            } else {
                StatusCode::NO_CONTENT
            }
        );
        assert!(
            !repeated.headers().contains_key(header::SET_COOKIE),
            "repeat scans retain the same browser session"
        );
    }
    assert_eq!(f.status().await, before);
    f.server.state.access.data.lock().unwrap().attempts = 64;
    assert_eq!(
        f.pair(ip, ip, &"a".repeat(64)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn pairing_code_is_local_only_and_new_run_rejects_previous_code_and_cookie() {
    let f = Fixture::new().await;
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let code = f.pairing().await;
    let paired = cookie(&f.pair(ip, ip, token(&code)).await);
    let path = format!("{}/workbench/v1/access/pairing", f.server.state.origin);
    assert_eq!(
        f.client.get(&path).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.client
            .get(&path)
            .header(header::COOKIE, &f.owner)
            .header(header::ORIGIN, "https://untrusted.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let response = f
        .client
        .get(&path)
        .header(header::COOKIE, &f.owner)
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    drop(f);
    let next = Fixture::new().await;
    assert_eq!(next.status().await["state"], "off");
    let ready = next.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let new_code = next.pairing().await;
    assert_ne!(token(&code), token(&new_code));
    assert_eq!(
        next.pair(ip, ip, token(&code)).await.status(),
        StatusCode::GONE
    );
    assert_eq!(
        next.client
            .get(format!("{ip}/workbench/v1/run"))
            .header(header::COOKIE, paired)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
#[ignore = "waits 301 real seconds to cross the old pairing deadline; isolated fixture only"]
async fn pairing_code_remains_usable_after_five_real_minutes() {
    let f = Fixture::new().await;
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let original = f.pairing().await;
    assert_eq!(
        f.pair(ip, ip, token(&original)).await.status(),
        StatusCode::NO_CONTENT
    );
    tokio::time::sleep(Duration::from_secs(301)).await;
    assert_eq!(token(&f.pairing().await), token(&original));
    assert_eq!(
        f.pair(ip, ip, token(&original)).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(f.status().await["devices"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn saved_port_is_read_on_enable_and_busy_port_fails_without_touching_local_cli() {
    use crate::workbench::config::Config;
    let occupied = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = occupied.local_addr().unwrap().port();
    let f = Fixture::with_config(Some(
        Config::parse(format!(r#"{{"schemaVersion":1,"access":{{"port":{port}}}}}"#).as_bytes())
            .unwrap(),
    ))
    .await;
    assert_eq!(f.status().await["state"], "off");
    assert_eq!(f.action("enable").await.status(), StatusCode::ACCEPTED);
    for _ in 0..100 {
        if f.status().await["state"] == "error" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.status().await["error"], "listen_failed");
    // A saved preference is not an auto-open switch. Retrying uses the saved port.
    drop(occupied);
    assert_eq!(f.ready().await["port"], port);
    assert!(f._host.handle().attach().await.is_ok());
}

async fn json_response(response: reqwest::Response) -> Value {
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed Chrome; isolated shell PTY and synthetic device addresses"]
async fn browser_device_access_qr_http_stream_input_and_revocation() {
    use crate::workbench::decode::{Change, Decoded, TextKey};
    use crate::workbench::redaction::RedactionPolicy;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let f = Fixture::new().await;
    let mut command = tokio::process::Command::new("node");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/e2e/r5-device-access-probe.cjs"
        ))
        .env("WORKBENCH_PROBE_URL", f.server.bootstrap_url())
        .env("DEBUG", "")
        .env("PWDEBUG", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("WORKBENCH_TEST_SCREENSHOT") {
        command.env("WORKBENCH_PROBE_SCREENSHOT", path);
    }
    let mut browser = crate::workbench::probe_process::ProbeProcess::spawn(&mut command).unwrap();
    let _liveness = browser.stdin.take().unwrap();
    let mut lines = BufReader::new(browser.stdout.take().unwrap()).lines();
    let mut complete = false;
    tokio::time::timeout(Duration::from_secs(55), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let value: Value = serde_json::from_str(&line).unwrap();
            println!("{value}");
            if value["stage"] == "complete" { complete = true; }
            else {
                assert_eq!(value["stage"], "progress");
                if value["check"] == "phone-stream" {
                    let request_id = Uuid::new_v4();
                    let policy = RedactionPolicy::new(vec![]).unwrap();
                    f.server.state.hub.apply(Decoded { request_id, capture_seq: 1, received_at: Instant::now(), change: Change::Request {
                        info: crate::workbench::decode::request::RequestInfo { client_request_index: None, requested_model: Some(policy.scrub("synthetic-model")), codex_thread_id: Some(Uuid::new_v4()), codex_turn_id: Some("phone-test".into()), purpose: crate::workbench::decode::request::RequestPurpose::Conversation, purpose_basis: crate::workbench::decode::request::PurposeBasis::CodexTurnMetadata }
                    }});
                    f.server.state.hub.apply(Decoded { request_id, capture_seq: 2, received_at: Instant::now(), change: Change::TextDelta {
                        key: TextKey { request_id, response_id: Some("synthetic-response".into()), wire_item_id: "message".into(), content_index: 0 }, text: policy.scrub("手机流式中间内容：同一个运行与终端。")
                    }});
                }
            }
        }
        assert!(browser.wait().await.unwrap().success());
    }).await.unwrap();
    assert!(complete);
    let inputs = std::fs::read_to_string(f._dir.path().join("inputs")).unwrap();
    assert_eq!(inputs.matches("手机中文输入").count(), 1);
    assert_eq!(inputs.matches("电脑继续输入").count(), 1);
}

#[tokio::test]
async fn stopped_run_cannot_reenable_devices_but_owner_can_read_it() {
    let f = Fixture::new().await;
    let ready = f.ready().await;
    let ip = ready["addresses"][0]["origin"].as_str().unwrap();
    let pairing = f.pairing().await;
    let device = cookie(&f.pair(ip, ip, token(&pairing)).await);
    let terminal = f._host.handle();
    terminal.stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !terminal.exited() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The Application exit transition revokes access once and retains the
    // Run's desktop reader. A later enable must not resurrect authorization.
    f.server.state.access.close();
    assert!(
        f.server
            .state
            .access
            .permission(
                &{
                    let mut headers = HeaderMap::new();
                    headers.insert(header::COOKIE, device.parse().unwrap());
                    headers
                },
                f.server.state.hub.epoch()
            )
            .is_none()
    );
    let response = f.action("enable").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(json_response(response).await["error"]["code"], "run_ended");
    assert_ne!(f.status().await["state"], "ready");
    let reading = f
        .client
        .get(format!("{}/workbench/v1/run", f.server.state.origin))
        .header(header::COOKIE, &f.owner)
        .send()
        .await
        .unwrap();
    assert_eq!(reading.status(), StatusCode::OK);
}
