//! Synthetic wire tests: no real bot, native process, user home, or private data.
use super::*;
use crate::workbench::{
    channel::native::{Relay, RelayLimits},
    launch::{ControlHandle, NativeAttachment},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixListener, UnixStream},
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, client_async,
    tungstenite::{Message, protocol::Role},
};

type Socket = WebSocketStream<UnixStream>;
const BOT: i64 = 101;
const CHAT: i64 = 202;
const BODY: &str = "synthetic incoming private prompt";
const FINAL: &str = "synthetic final answer only";
fn thread() -> Uuid {
    Uuid::from_u128(2)
}
fn update(id: i64, message: i64, text: &str) -> Value {
    json!({"update_id":id,"message":{"message_id":message,"date":1,
        "from":{"id":CHAT,"is_bot":false},"chat":{"id":CHAT,"type":"private"},"text":text}})
}
async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn read(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("synthetic wire deadline")
}

struct Bot {
    client: Arc<Client>,
    requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}
impl Bot {
    async fn start(unknown: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            Client::local(listener.local_addr().unwrap(), Duration::from_secs(1)).unwrap(),
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            let mut next_message_id = 77;
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let end = loop {
                    let mut buffer = [0; 4096];
                    let size = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    assert!(bytes.len() < 65536);
                    if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let length = String::from_utf8_lossy(&bytes[..end])
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < end + length {
                    let mut buffer = [0; 4096];
                    let size = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(size, 0);
                    bytes.extend_from_slice(&buffer[..size]);
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes[end..end + length]).unwrap());
                if unknown {
                    continue;
                }
                let callback_ack = String::from_utf8_lossy(&bytes[..end])
                    .lines()
                    .next()
                    .is_some_and(|line| line.contains("/answerCallbackQuery "));
                let body = if callback_ack {
                    json!({"ok":true,"result":true})
                } else {
                    let id = next_message_id;
                    next_message_id += 1;
                    json!({"ok":true,"result":{"message_id":id,"chat":{"id":CHAT,"type":"private"}}})
                }.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            client,
            requests,
            task,
        }
    }
}
impl Drop for Bot {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Fixture {
    // Service drops first; temporary storage is exclusively fixture-owned.
    service: Service,
    session: session::Session,
    _relay: Relay,
    native: Socket,
    tui: Socket,
    control: Socket,
    driver: JoinHandle<()>,
    bot: Bot,
    root: tempfile::TempDir,
    next_pair: i64,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl Fixture {
    async fn new(unknown: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = root.path().canonicalize().unwrap();
        let listener = UnixListener::bind(parent.join("up.sock")).unwrap();
        let (relay, capture) = Relay::bind(
            parent.join("front.sock"),
            parent.join("up.sock"),
            RelayLimits::default(),
        )
        .await
        .unwrap();
        let connect = async {
            client_async(
                "ws://localhost/rpc",
                UnixStream::connect(relay.path()).await.unwrap(),
            )
            .await
            .unwrap()
            .0
        };
        let accept = async {
            accept_async(listener.accept().await.unwrap().0)
                .await
                .unwrap()
        };
        let (mut tui, mut native) = tokio::join!(connect, accept);
        let (client, server) = UnixStream::pair().unwrap();
        let client = Socket::from_raw_socket(client, Role::Client, None).await;
        let control = Socket::from_raw_socket(server, Role::Server, None).await;
        let (handle, driver) = ControlHandle::test_socket(client);
        let session = session::Session::start(
            Uuid::from_u128(1),
            NativeAttachment {
                capture,
                control: handle,
            },
        );
        send(
            &mut tui,
            json!({"id":1,"method":"thread/start","params":{}}),
        )
        .await;
        assert_eq!(read(&mut native).await["method"], "thread/start");
        send(&mut native,json!({"id":1,"result":{"thread":{"id":thread(),"source":"cli","status":{"type":"idle"}}}})).await;
        read(&mut tui).await;
        bounded(async {
            loop {
                if session.handle().qualified(thread()).is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let bot = Bot::start(unknown).await;
        let service = Service::new(parent.join("receipts"));
        service.register(session.handle());
        {
            let mut inner = service.core.inner.lock().unwrap();
            inner.binding.replace_bot(BotIdentity { id: BOT }).unwrap();
            inner.client = Some(bot.client.clone());
        }
        let mut fixture = Self {
            service,
            session,
            _relay: relay,
            native,
            tui,
            control,
            driver,
            bot,
            root,
            next_pair: 1000,
        };
        fixture.bind().await;
        fixture
    }
    async fn bind(&mut self) {
        let code = self.service.begin_pair().unwrap();
        self.next_pair += 1;
        process_update(
            &self.service.core,
            &self.bot.client,
            BOT,
            code.generation,
            &update(self.next_pair, self.next_pair, &code.command),
        )
        .await
        .unwrap();
        let candidate = self.service.snapshot().binding.candidate.unwrap();
        let source = self.session.snapshot();
        self.service
            .confirm(candidate, source.run_id, thread(), source.revision, false)
            .unwrap();
    }
    async fn admit(&self, update_id: i64, message_id: i64) {
        let generation = self.service.snapshot().binding.generation;
        process_update(
            &self.service.core,
            &self.bot.client,
            BOT,
            generation,
            &update(update_id, message_id, BODY),
        )
        .await
        .unwrap();
    }
    fn pop(&self) -> Job {
        self.service
            .core
            .inner
            .lock()
            .unwrap()
            .queue
            .pop_front()
            .unwrap()
    }
    async fn complete(&mut self, job: &Job, local_input: bool) {
        let core = self.service.core.clone();
        let wire = async {
            let request = read(&mut self.control).await;
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["params"]["input"][0]["text"], BODY);
            let client = request["params"]["clientUserMessageId"].clone();
            let turn = Uuid::from_u128(3);
            send(
                &mut self.control,
                json!({"id":request["id"],"result":{"turn":{"id":turn}}}),
            )
            .await;
            if local_input {
                send(
                    &mut self.tui,
                    json!({"id":9,"method":"turn/steer","params":{"threadId":thread(),"input":[]}}),
                )
                .await;
                assert_eq!(read(&mut self.native).await["method"], "turn/steer");
            }
            for value in [
                json!({"method":"turn/started","params":{"threadId":thread(),"turn":{"id":turn,"status":"inProgress"}}}),
                json!({"method":"item/started","params":{"threadId":thread(),"turnId":turn,"item":{"type":"userMessage","id":"u","clientId":client,"content":[]}}}),
                json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"userMessage","id":"u","clientId":client,"content":[]}}}),
                json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"agentMessage","id":"comment","phase":"commentary","text":"synthetic commentary must stay local"}}}),
                json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"reasoning","id":"reason","text":"synthetic reasoning must stay local"}}}),
                json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"agentMessage","id":"final","phase":"final_answer","text":FINAL}}}),
                json!({"method":"turn/completed","params":{"threadId":thread(),"turn":{"id":turn,"status":"completed"}}}),
            ] {
                send(&mut self.native, value.clone()).await;
                assert_eq!(read(&mut self.tui).await, value);
            }
        };
        bounded(async {
            tokio::join!(execute_job(&core, job), wire);
        })
        .await;
    }
}

#[tokio::test]
async fn private_receipt_dedup_and_final_only_wire_delivery() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    f.admit(99, 2).await;
    assert_eq!(f.service.snapshot().queued, 1);
    let journal = fs::read_to_string(f.root.path().join("receipts/receipts.jsonl")).unwrap();
    assert!(!journal.contains(BODY));
    assert!(!journal.contains("synthetic-secret"));
    let job = f.pop();
    f.complete(&job, false).await;
    let view = f.service.snapshot().last_task.unwrap();
    assert_eq!(view.delivery, "confirmed");
    assert!(view.native_confirmed && view.native_completed);
    {
        let requests = f.bot.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["text"], FINAL);
        assert_eq!(requests[0]["chat_id"], CHAT);
        assert!(requests[0].get("parse_mode").is_none());
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.control.next())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn real_local_input_withholds_remote_reply() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    f.complete(&job, true).await;
    assert_eq!(f.service.snapshot().last_task.unwrap().delivery, "withheld");
    assert!(f.bot.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn uncertain_bot_send_is_attempted_once() {
    let mut f = Fixture::new(true).await;
    f.admit(2, 2).await;
    let job = f.pop();
    f.complete(&job, false).await;
    assert_eq!(f.service.snapshot().last_task.unwrap().delivery, "unknown");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn revoked_queue_and_old_poll_window_never_submit_native() {
    let mut f = Fixture::new(false).await;
    let old = f.service.snapshot().binding.generation;
    f.admit(2, 2).await;
    assert_eq!(f.service.revoke().queued, 0);
    f.bind().await;
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        old,
        &update(3, 3, BODY),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.control.next())
            .await
            .is_err()
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
    f.admit(4, 4).await;
    assert_eq!(f.service.snapshot().queued, 1);
}

#[tokio::test]
async fn rejected_update_cannot_be_reinterpreted_after_new_binding_bot_switch_or_restart() {
    let mut f = Fixture::new(false).await;
    let old = f.service.snapshot().binding.generation;
    f.service.revoke();
    f.bind().await;
    // This update is explicitly processed and rejected in an old window.
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        old,
        &update(30, 30, BODY),
    )
    .await
    .unwrap();
    // Re-delivery can happen before Telegram receives the next offset request.
    // It must not acquire the newly granted authorization.
    f.admit(30, 30).await;
    assert_eq!(
        f.service.snapshot().queued,
        0,
        "an explicitly rejected update was reinterpreted"
    );
    {
        let mut inner = f.service.core.inner.lock().unwrap();
        inner
            .binding
            .replace_bot(BotIdentity { id: BOT + 1 })
            .unwrap();
        inner.binding.replace_bot(BotIdentity { id: BOT }).unwrap();
    }
    f.bind().await;
    f.admit(31, 30).await;
    assert_eq!(f.service.snapshot().queued, 0);
    // Release the one writer before creating a fresh Service with no binding.
    f.service.core.worker.lock().unwrap().take();
    let path = f.root.path().canonicalize().unwrap().join("receipts");
    bounded(async {
        loop {
            match super::super::store::Store::open(&path) {
                Ok(store) => {
                    drop(store);
                    break;
                }
                Err(super::super::store::Failure::Busy) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("unexpected static store failure: {error}"),
            }
        }
    })
    .await;
    let replacement = Service::new(path.clone());
    let previous = std::mem::replace(&mut f.service, replacement);
    drop(previous);
    f.service.register(f.session.handle());
    {
        let mut inner = f.service.core.inner.lock().unwrap();
        inner.binding.replace_bot(BotIdentity { id: BOT }).unwrap();
        inner.client = Some(f.bot.client.clone());
    }
    f.bind().await;
    f.admit(32, 30).await;
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(
        f.service
            .core
            .writer()
            .unwrap()
            .load()
            .await
            .unwrap()
            .recovered,
        Counts::default()
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.control.next())
            .await
            .is_err()
    );
    let journal = fs::read_to_string(path.join("receipts.jsonl")).unwrap();
    assert!(!journal.contains(BODY));
    assert!(!journal.contains("synthetic-secret"));
}

#[tokio::test]
async fn uncertain_native_submission_keeps_durable_intent_and_never_retries() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let task = tokio::spawn(execute(f.service.core.clone()));
    f.service.tasks.lock().unwrap().push(task);
    let request = read(&mut f.control).await;
    // The peer received the native request but returns an invalid ACK. There is
    // no safe conclusion about whether the native operation actually executed.
    send(&mut f.control, json!({"id":request["id"],"result":{}})).await;
    bounded(async {
        loop {
            if f.service
                .snapshot()
                .last_task
                .is_some_and(|task| task.phase == "uncertain")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let counts = f
        .service
        .core
        .writer()
        .unwrap()
        .load()
        .await
        .unwrap()
        .recovered;
    assert_eq!(
        counts,
        Counts {
            received: 0,
            submitting: 1,
            closed: 0
        }
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
    f.admit(3, 2).await;
    assert_eq!(f.service.snapshot().queued, 0);
    for task in f.service.tasks.lock().unwrap().drain(..) {
        task.abort();
    }
    f.service.core.worker.lock().unwrap().take();
    let path = f.root.path().join("receipts");
    let mut reopened = bounded(async {
        loop {
            match super::super::store::Store::open(&path) {
                Ok(store) => break store,
                Err(super::super::store::Failure::Busy) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("unexpected static store failure: {error}"),
            }
        }
    })
    .await;
    let recovered = reopened.recover().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].1, super::super::store::Status::Submitting);
}

#[tokio::test]
async fn native_busy_wait_allows_revocation_without_submitting() {
    let mut f = Fixture::new(false).await;
    send(&mut f.native, json!({"method":"thread/status/changed","params":{"threadId":thread(),"status":{"type":"active","activeFlags":[]}}})).await;
    read(&mut f.tui).await;
    bounded(async {
        loop {
            if f.session
                .snapshot()
                .targets
                .iter()
                .any(|target| target.thread_id == thread() && target.busy)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    f.admit(2, 2).await;
    let job = f.pop();
    let core = f.service.core.clone();
    let execute = execute_job(&core, &job);
    let revoke = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.service.revoke();
    };
    bounded(async {
        tokio::join!(execute, revoke);
    })
    .await;
    assert_eq!(f.service.snapshot().last_task.unwrap().phase, "abandoned");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.control.next())
            .await
            .is_err()
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
    assert_eq!(
        f.service
            .core
            .writer()
            .unwrap()
            .load()
            .await
            .unwrap()
            .recovered
            .submitting,
        0
    );
}

#[tokio::test]
async fn local_confirmation_requires_the_same_revision_as_atomic_target_qualification() {
    let mut f = Fixture::new(false).await;
    let code = f.service.begin_pair().unwrap();
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        code.generation,
        &update(2000, 2000, &code.command),
    )
    .await
    .unwrap();
    let candidate = f.service.snapshot().binding.candidate.unwrap();
    let old_source = f.session.snapshot();
    let old_target = f
        .session
        .handle()
        .qualified_at(thread(), old_source.revision)
        .unwrap();
    send(&mut f.native, json!({"method":"thread/status/changed","params":{"threadId":thread(),"status":{"type":"active","activeFlags":[]}}})).await;
    read(&mut f.tui).await;
    let current = bounded(async {
        loop {
            let source = f.session.snapshot();
            if source.revision != old_source.revision
                && source
                    .targets
                    .iter()
                    .any(|target| target.thread_id == thread() && target.busy)
            {
                break source;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        f.session
            .handle()
            .qualified_at(thread(), old_source.revision),
        Err(session::Failure::SourceChanged)
    );
    assert!(matches!(
        f.service.confirm(
            candidate,
            old_source.run_id,
            thread(),
            old_source.revision,
            false,
        ),
        Err("channel_source_changed")
    ));
    let waiting = f.service.snapshot();
    assert_eq!(waiting.binding.phase, Phase::AwaitingConfirmation);
    assert!(waiting.binding.active.is_none());
    let confirmed = f
        .service
        .confirm(candidate, current.run_id, thread(), current.revision, false)
        .unwrap();
    let active = confirmed.binding.active.unwrap();
    assert_eq!(active.target, old_target);
    assert_eq!(f.session.handle().ready(active.target), Ok(false));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.control.next())
            .await
            .is_err()
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pairing_and_confirmation_cancel_old_poll_without_losing_first_new_message() {
    let f = Fixture::new(false).await;
    f.service.revoke();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client =
        Arc::new(Client::local(listener.local_addr().unwrap(), Duration::from_secs(3)).unwrap());
    let (requests, mut received) = tokio::sync::mpsc::channel(8);
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    let requests = requests.clone();
                    connections.spawn(async move {
                        let mut bytes = Vec::new();
                        let end = loop {
                            let mut buffer = [0;4096];
                            let size = stream.read(&mut buffer).await.unwrap();
                            if size == 0 { return; }
                            bytes.extend_from_slice(&buffer[..size]);
                            assert!(bytes.len()<65536);
                            if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") { break end+4; }
                        };
                        let length = String::from_utf8_lossy(&bytes[..end]).lines().find_map(|line| {
                            let (name,value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                        }).unwrap();
                        while bytes.len() < end+length {
                            let mut buffer = [0;4096];
                            let size = stream.read(&mut buffer).await.unwrap();
                            if size == 0 { return; }
                            bytes.extend_from_slice(&buffer[..size]);
                        }
                        let request: Value = serde_json::from_slice(&bytes[end..end+length]).unwrap();
                        let (reply,result) = tokio::sync::oneshot::channel::<Value>();
                        if requests.send((request,reply)).await.is_err() { return; }
                        let Ok(result) = result.await else { return; };
                        let body = json!({"ok":true,"result":result}).to_string();
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                        let _ = stream.write_all(response.as_bytes()).await;
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    // The fixture's Service owns this server task solely for bounded test cleanup.
    f.service.tasks.lock().unwrap().push(server);
    f.service.core.inner.lock().unwrap().client = Some(client);
    let polling = tokio::spawn(poll(f.service.core.clone()));
    f.service.tasks.lock().unwrap().push(polling);
    let (_, old_idle) = bounded(received.recv()).await.unwrap();
    let pairing = f.service.begin_pair().unwrap();
    let (request, paired) = bounded(received.recv()).await.unwrap();
    assert!(request.get("offset").is_none_or(Value::is_null));
    drop(old_idle);
    paired
        .send(json!([update(10, 10, &pairing.command)]))
        .unwrap();
    let (request, old_candidate) = bounded(received.recv()).await.unwrap();
    assert_eq!(request["offset"], 11);
    let candidate = f.service.snapshot().binding.candidate.unwrap();
    let source = f.session.snapshot();
    f.service
        .confirm(candidate, source.run_id, thread(), source.revision, false)
        .unwrap();
    let (request, active) = bounded(received.recv()).await.unwrap();
    assert_eq!(request["offset"], 11);
    drop(old_candidate);
    active.send(json!([update(11, 11, BODY)])).unwrap();
    let (request, pending) = bounded(received.recv()).await.unwrap();
    assert_eq!(request["offset"], 12);
    assert_eq!(f.service.snapshot().queued, 1);
    assert_eq!(
        f.service
            .core
            .writer()
            .unwrap()
            .load()
            .await
            .unwrap()
            .recovered
            .received,
        1
    );
    drop(pending);
}

// P2 fixtures explicitly opt in; existing text-only tests keep consent false.
impl Fixture {
    async fn bind_with_consent(&mut self) {
        let code = self.service.begin_pair().unwrap();
        self.next_pair += 1;
        process_update(
            &self.service.core,
            &self.bot.client,
            BOT,
            code.generation,
            &update(self.next_pair, self.next_pair, &code.command),
        )
        .await
        .unwrap();
        let candidate = self.service.snapshot().binding.candidate.unwrap();
        let source = self.session.snapshot();
        self.service
            .confirm(candidate, source.run_id, thread(), source.revision, true)
            .unwrap();
    }

    async fn owned_interaction_job(&mut self) -> (Job, session::Attempt, interactions::Cleanup) {
        self.admit(20, 20).await;
        let job = self.pop();
        let client = Uuid::new_v4();
        let mut attempt = job.session.arm(job.active.target, client).unwrap();
        self.service
            .core
            .writer()
            .unwrap()
            .mark_submitting(job.id, client)
            .await
            .unwrap();
        let wire = async {
            let request = read(&mut self.control).await;
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["params"]["clientUserMessageId"], client.to_string());
            send(
                &mut self.control,
                json!({"id":request["id"],"result":{"turn":{"id":interaction_turn()}}}),
            )
            .await;
            for value in [
                json!({"method":"turn/started","params":{"threadId":thread(),"turn":{"id":interaction_turn(),"status":"inProgress"}}}),
                json!({"method":"item/started","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
                json!({"method":"item/completed","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
            ] {
                send(&mut self.native, value.clone()).await;
                assert_eq!(read(&mut self.tui).await, value);
            }
        };
        let (result, ()) = bounded(async { tokio::join!(attempt.submit(BODY.into()), wire) }).await;
        assert_eq!(
            result,
            Submission::Accepted {
                turn_id: interaction_turn()
            }
        );
        bounded(async {
            while attempt.revalidate().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        self.service
            .core
            .update(&job, "running", true, false, "pending", None);
        let cleanup = interactions::Cleanup::new(
            self.service.core.clone(),
            job.id,
            job.active,
            attempt.interactions(),
        );
        (job, attempt, cleanup)
    }

    async fn native_interaction(&mut self, attempt: &session::Attempt, value: Value) {
        send(&mut self.native, value.clone()).await;
        assert_eq!(read(&mut self.tui).await, value);
        bounded(async {
            while attempt.revalidate().is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    async fn offer_interaction(&self, job: &Job, attempt: &session::Attempt) {
        bounded(interactions::offer(
            &self.service.core,
            job,
            &attempt.interactions(),
        ))
        .await;
    }

    async fn process_callback(&self, value: &Value) {
        let generation = self.service.snapshot().binding.generation;
        bounded(process_update(
            &self.service.core,
            &self.bot.client,
            BOT,
            generation,
            value,
        ))
        .await
        .unwrap();
    }

    async fn no_native_answer(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(30), self.control.next())
                .await
                .is_err()
        );
    }

    fn prompt(&self, index: usize) -> Value {
        self.bot
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.get("reply_markup").is_some())
            .nth(index)
            .unwrap()
            .clone()
    }

    fn journal(&self) -> String {
        fs::read_to_string(self.root.path().join("receipts/receipts.jsonl")).unwrap()
    }
}

fn interaction_turn() -> Uuid {
    Uuid::from_u128(30)
}
fn command_interaction(id: Value) -> Value {
    json!({"id":id,"method":"item/commandExecution/requestApproval","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"synthetic-command-item",
        "command":"printf SYNTHETIC_APPROVAL_BODY","reason":"Synthetic permission reason",
        "cwd":"/private/SYNTHETIC_CWD","availableDecisions":["accept","decline"]
    }})
}
fn button_reference(prompt: &Value, label: &str) -> String {
    prompt["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
        .find(|button| button["text"] == label)
        .unwrap()["callback_data"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn callback_update(update: i64, query: &str, reference: &str) -> Value {
    json!({"update_id":update,"callback_query":{"id":query,
        "from":{"id":CHAT,"is_bot":false},"data":reference,
        "message":{"message_id":77,"date":1,"from":{"id":BOT,"is_bot":true},"chat":{"id":CHAT,"type":"private"}}}})
}
fn callback_receipts(journal: &str) -> usize {
    journal
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["record"]["event"]["kind"] == "callback_received")
        .count()
}

fn question_reply_update(
    update_id: i64,
    message_id: i64,
    prompt_message_id: i64,
    text: &str,
) -> Value {
    let mut value = update(update_id, message_id, text);
    value["message"]["reply_to_message"] = json!({"message_id":prompt_message_id,"date":1,
        "from":{"id":BOT,"is_bot":true},"chat":{"id":CHAT,"type":"private"},
        "text":"SYNTHETIC_QUOTED_BODY_MUST_NOT_BE_RETAINED"});
    value
}

fn question_reply_receipts(journal: &str) -> usize {
    journal
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["record"]["event"]["kind"] == "question_reply_received")
        .count()
}

fn one_question(id: Value, options: Value, is_other: bool) -> Value {
    json!({"id":id,"method":"item/tool/requestUserInput","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item","questions":[{
            "id":"original-question","header":"Synthetic","question":"SYNTHETIC_FREE_QUESTION",
            "isOther":is_other,"isSecret":false,"options":options}]}})
}

#[tokio::test]
async fn interaction_mixed_questions_bind_text_to_current_prompt_and_never_queue_replies() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt,json!({"id":"mixed-rpc","method":"item/tool/requestUserInput","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item","questions":[
            {"id":"first-id","header":"First","question":"SYNTHETIC_FIRST_QUESTION","isOther":true,"isSecret":false,
             "options":[{"label":"First A","description":"A"},{"label":"First B","description":"B"}]},
            {"id":"second-id","header":"Second","question":"SYNTHETIC_SECOND_QUESTION","isOther":true,"isSecret":false,
             "options":[{"label":"Second A","description":"A"},{"label":"Second B","description":"B"}]}
        ]}})).await;
    f.offer_interaction(&job, &attempt).await;
    assert!(
        !f.prompt(0)["text"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC_SECOND_QUESTION")
    );
    let first = button_reference(&f.prompt(0), "1.1");
    f.process_callback(&callback_update(21, "SYNTHETIC_MIXED_FIRST", &first))
        .await;
    f.no_native_answer().await;
    assert!(
        !f.prompt(1)["text"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC_FIRST_QUESTION")
    );
    let generation = f.service.snapshot().binding.generation;
    for value in [
        question_reply_update(22, 22, 77, "SYNTHETIC_OLD_QUESTION_REPLY"),
        question_reply_update(23, 23, 999, "SYNTHETIC_WRONG_PROMPT_REPLY"),
        update(24, 24, "SYNTHETIC_UNATTACHED_TEXT"),
    ] {
        process_update(&f.service.core, &f.bot.client, BOT, generation, &value)
            .await
            .unwrap();
    }
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(question_reply_receipts(&f.journal()), 0);
    f.no_native_answer().await;
    let text = "SYNTHETIC_OTHER_ANSWER 😀\n保留原文";
    let reply = question_reply_update(25, 25, 78, text);
    process_update(&f.service.core, &f.bot.client, BOT, generation, &reply)
        .await
        .unwrap();
    assert_eq!(question_reply_receipts(&f.journal()), 1);
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"mixed-rpc","result":{"answers":{
        "first-id":{"answers":["First A"]},"second-id":{"answers":[text]}}}})
    );
    assert_eq!(attempt.interactions().awaiting_question(), Ok(true));
    for value in [
        reply.clone(),
        question_reply_update(26, 25, 78, text),
        question_reply_update(27, 27, 78, text),
    ] {
        process_update(&f.service.core, &f.bot.client, BOT, generation, &value)
            .await
            .unwrap();
    }
    assert_eq!(question_reply_receipts(&f.journal()), 1);
    assert_eq!(f.service.snapshot().queued, 0);
    f.no_native_answer().await;
    for private in [
        text,
        "SYNTHETIC_FIRST_QUESTION",
        "SYNTHETIC_SECOND_QUESTION",
        "SYNTHETIC_QUOTED_BODY_MUST_NOT_BE_RETAINED",
        "First A",
        &first,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_pure_text_question_requires_reply_and_preserves_unicode() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, one_question(json!(83), Value::Null, false))
        .await;
    f.offer_interaction(&job, &attempt).await;
    assert_eq!(f.prompt(0)["reply_markup"]["force_reply"], true);
    let generation = f.service.snapshot().binding.generation;
    for (i, text) in [
        (21, " "),
        (22, "\n\n"),
        (23, "SYNTHETIC\u{202e}"),
        (24, "x"),
    ] {
        let mut value = question_reply_update(i, i, 77, text);
        if i == 24 {
            value["message"]["reply_to_message"]["from"]["id"] = json!(BOT + 1);
        }
        process_update(&f.service.core, &f.bot.client, BOT, generation, &value)
            .await
            .unwrap();
    }
    assert_eq!(f.service.core.inner.lock().unwrap().interactions.len(), 1);
    assert_eq!(question_reply_receipts(&f.journal()), 0);
    f.no_native_answer().await;
    let text = "  SYNTHETIC_FREE_TEXT😀\n第二行  ";
    let reply = question_reply_update(25, 25, 77, text);
    process_update(&f.service.core, &f.bot.client, BOT, generation, &reply)
        .await
        .unwrap();
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":83,"result":{"answers":{
        "original-question":{"answers":[text]}}}})
    );
    assert_eq!(question_reply_receipts(&f.journal()), 1);
    assert_eq!(f.service.snapshot().queued, 0);
    assert!(!f.journal().contains("SYNTHETIC_FREE_TEXT"));
}

#[tokio::test]
async fn interaction_question_reply_is_withheld_after_native_retirement_expiry_or_revoke() {
    for mode in ["resolved", "local", "expired", "revoke", "choiceOnly"] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        let options = if mode == "choiceOnly" {
            json!([{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}])
        } else {
            Value::Null
        };
        f.native_interaction(&attempt, one_question(json!(83), options, false))
            .await;
        f.offer_interaction(&job, &attempt).await;
        let id = *f
            .service
            .core
            .inner
            .lock()
            .unwrap()
            .interactions
            .keys()
            .next()
            .unwrap();
        let generation = f.service.snapshot().binding.generation;
        match mode {
            "resolved" => {
                f.native_interaction(
                    &attempt,
                    json!({"method":"serverRequest/resolved","params":{
                "threadId":thread(),"requestId":83}}),
                )
                .await
            }
            "local" => {
                let answer = json!({"id":83,"result":{"answers":{}}});
                send(&mut f.tui, answer.clone()).await;
                assert_eq!(read(&mut f.native).await, answer);
            }
            "expired" => attempt.interactions().expire_for_test(id),
            "revoke" => {
                f.service.revoke();
            }
            _ => {}
        }
        if matches!(mode, "resolved" | "local") {
            bounded(async {
                while attempt.interactions().current(id).is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
        }
        let value = question_reply_update(21, 21, 77, "SYNTHETIC_STALE_TEXT");
        process_update(&f.service.core, &f.bot.client, BOT, generation, &value)
            .await
            .unwrap();
        assert_eq!(question_reply_receipts(&f.journal()), 0);
        assert_eq!(f.service.snapshot().queued, 0);
        f.no_native_answer().await;
    }
}

#[tokio::test]
async fn interaction_reply_before_prompt_confirmation_or_unknown_send_has_no_authority() {
    let mut f = Fixture::new(true).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, one_question(json!(83), Value::Null, false))
        .await;
    let core = f.service.core.clone();
    let handle = attempt.interactions();
    let mut offering = Some(tokio::spawn(async move {
        interactions::offer(&core, &job, &handle).await
    }));
    bounded(async {
        while f.bot.requests.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let generation = f.service.snapshot().binding.generation;
    for i in [21, 22] {
        if i == 22 {
            bounded(async {
                offering.take().unwrap().await.unwrap();
            })
            .await;
        }
        process_update(
            &f.service.core,
            &f.bot.client,
            BOT,
            generation,
            &question_reply_update(i, i, 77, "SYNTHETIC_UNCONFIRMED_REPLY"),
        )
        .await
        .unwrap();
        assert_eq!(question_reply_receipts(&f.journal()), 0);
        assert_eq!(f.service.snapshot().queued, 0);
    }
    f.no_native_answer().await;
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
}

#[tokio::test]
async fn interaction_confirmed_prompt_callback_is_durable_once_and_preserves_rpc_id() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, command_interaction(json!("73")))
        .await;
    f.offer_interaction(&job, &attempt).await;
    let prompt = f.prompt(0);
    assert!(
        prompt["text"]
            .as_str()
            .unwrap()
            .contains("SYNTHETIC_APPROVAL_BODY")
    );
    assert!(!prompt.to_string().contains("SYNTHETIC_CWD"));
    assert_eq!(prompt["chat_id"], CHAT);
    assert!(prompt.get("parse_mode").is_none());
    let reference = button_reference(&prompt, "Allow once");
    let deny = button_reference(&prompt, "Deny");
    assert_eq!(reference.len(), 32);
    assert_eq!(f.service.core.inner.lock().unwrap().interactions.len(), 1);
    let callback = callback_update(21, "SYNTHETIC_QUERY_PRIVATE", &reference);
    f.process_callback(&callback).await;
    // Native reading happens after callback receipt fsync and the write attempt.
    let journal = f.journal();
    assert_eq!(callback_receipts(&journal), 1);
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"73","result":{"decision":"accept"}})
    );
    assert_eq!(
        f.service.snapshot().last_task.unwrap().reason,
        Some("channel_interaction_written_unconfirmed")
    );
    f.process_callback(&callback).await;
    f.process_callback(&callback_update(22, "SYNTHETIC_SIBLING_QUERY", &deny))
        .await;
    f.no_native_answer().await;
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert_eq!(
        f.bot
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("callback_query_id").is_some())
            .count(),
        1
    );
    for private in [
        BODY,
        "SYNTHETIC_APPROVAL_BODY",
        "SYNTHETIC_CWD",
        "SYNTHETIC_QUERY_PRIVATE",
        &reference,
        &deny,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_local_answer_resolved_and_revoke_withhold_old_callbacks() {
    for invalidation in ["local", "resolved", "revoke"] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        f.native_interaction(&attempt, command_interaction(json!(73)))
            .await;
        f.offer_interaction(&job, &attempt).await;
        let reference = button_reference(&f.prompt(0), "Allow once");
        let id = *f
            .service
            .core
            .inner
            .lock()
            .unwrap()
            .interactions
            .keys()
            .next()
            .unwrap();
        match invalidation {
            "local" => {
                let answer = json!({"id":73,"result":{"decision":"decline"}});
                send(&mut f.tui, answer.clone()).await;
                assert_eq!(read(&mut f.native).await, answer);
            }
            "resolved" => {
                f.native_interaction(&attempt, json!({"method":"serverRequest/resolved","params":{"threadId":thread(),"requestId":73}})).await;
            }
            _ => {
                f.service.revoke();
            }
        }
        if invalidation != "revoke" {
            bounded(async {
                while attempt.interactions().current(id).is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
        }
        f.process_callback(&callback_update(21, "SYNTHETIC_LATE_QUERY", &reference))
            .await;
        f.no_native_answer().await;
        assert_eq!(callback_receipts(&f.journal()), 0);
        assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn interaction_questions_collect_all_choices_before_one_typed_native_answer() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, json!({"id":"questions-rpc","method":"item/tool/requestUserInput","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item","isBlocking":true,
        "questions":[
            {"id":"first-original","header":"First","question":"SYNTHETIC_QUESTION_ONE","isOther":false,"isSecret":false,
             "options":[{"label":"First A","description":"A"},{"label":"First B","description":"B"}]},
            {"id":"second-original","header":"Second","question":"SYNTHETIC_QUESTION_TWO","isOther":false,"isSecret":false,
             "options":[{"label":"Second A","description":"A"},{"label":"Second B","description":"B"}]}
        ]}})).await;
    f.offer_interaction(&job, &attempt).await;
    let first = button_reference(&f.prompt(0), "1.2");
    f.process_callback(&callback_update(21, "SYNTHETIC_QUESTION_QUERY_1", &first))
        .await;
    f.no_native_answer().await;
    let generation = f.service.snapshot().binding.generation;
    let text = update(22, 22, "SYNTHETIC_FREE_TEXT_MUST_NOT_QUEUE");
    process_update(&f.service.core, &f.bot.client, BOT, generation, &text)
        .await
        .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    assert!(
        f.service
            .core
            .writer()
            .unwrap()
            .is_rejected(
                BOT,
                22,
                Some(DeliveryKey {
                    bot_id: BOT,
                    chat_id: CHAT,
                    message_id: 22,
                })
            )
            .await
            .unwrap()
    );
    f.no_native_answer().await;
    assert!(f.bot.requests.lock().unwrap().iter().any(|request| {
        request["text"]
            .as_str()
            .is_some_and(|text| text.contains("not queued as a new task"))
    }));
    assert!(!f.journal().contains("SYNTHETIC_FREE_TEXT_MUST_NOT_QUEUE"));
    let second_prompt = f.prompt(1);
    assert!(
        second_prompt["text"]
            .as_str()
            .unwrap()
            .contains("Earlier answers recorded")
    );
    let second = button_reference(&second_prompt, "2.1");
    assert_ne!(first, second);
    let mut callback = callback_update(23, "SYNTHETIC_QUESTION_QUERY_2", &second);
    callback["callback_query"]["message"]["message_id"] = json!(78);
    f.process_callback(&callback).await;
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"questions-rpc","result":{"answers":{
            "first-original":{"answers":["First B"]},"second-original":{"answers":["Second A"]}
        }}})
    );
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 2);
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    for private in [
        "SYNTHETIC_QUESTION_ONE",
        "SYNTHETIC_QUESTION_TWO",
        "SYNTHETIC_QUESTION_QUERY_1",
        "First B",
        "Second A",
        &first,
        &second,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_expired_question_rejects_text_until_exact_native_retirement() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(
        &attempt,
        json!({"id":"expired-question-rpc","method":"item/tool/requestUserInput","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item",
        "questions":[{"id":"choice","header":"Choice","question":"SYNTHETIC_EXPIRED_QUESTION",
            "isOther":false,"isSecret":false,"options":[{"label":"Yes","description":"Continue"},
            {"label":"No","description":"Stop"}]}]}}),
    )
    .await;
    f.offer_interaction(&job, &attempt).await;
    let id = *f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .next()
        .unwrap();
    let handle = attempt.interactions();
    handle.expire_for_test(id);
    assert!(handle.current(id).is_err());
    assert_eq!(handle.awaiting_question(), Ok(true));
    f.process_callback(&callback_update(
        21,
        "SYNTHETIC_EXPIRED_QUERY",
        &button_reference(&f.prompt(0), "1.1"),
    ))
    .await;
    assert_eq!(callback_receipts(&f.journal()), 0);
    let generation = f.service.snapshot().binding.generation;
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        generation,
        &update(22, 22, "SYNTHETIC_EXPIRED_QUESTION_TEXT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    assert!(
        f.service
            .core
            .writer()
            .unwrap()
            .is_rejected(
                BOT,
                22,
                Some(DeliveryKey {
                    bot_id: BOT,
                    chat_id: CHAT,
                    message_id: 22,
                })
            )
            .await
            .unwrap()
    );
    assert!(!f.journal().contains("SYNTHETIC_EXPIRED_QUESTION_TEXT"));
    f.no_native_answer().await;
    f.native_interaction(
        &attempt,
        json!({"method":"serverRequest/resolved","params":{
        "threadId":thread(),"requestId":"unrelated-rpc"}}),
    )
    .await;
    assert_eq!(handle.awaiting_question(), Ok(true));
    f.native_interaction(
        &attempt,
        json!({"method":"serverRequest/resolved","params":{
        "threadId":thread(),"requestId":"expired-question-rpc"}}),
    )
    .await;
    assert_eq!(handle.awaiting_question(), Ok(false));
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        generation,
        &update(23, 23, "SYNTHETIC_NEXT_TASK_AFTER_RETIREMENT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 1);
}

#[tokio::test]
async fn interaction_without_separate_consent_exports_no_request_content() {
    let mut f = Fixture::new(false).await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, command_interaction(json!(73)))
        .await;
    f.offer_interaction(&job, &attempt).await;
    assert!(f.bot.requests.lock().unwrap().is_empty());
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    f.no_native_answer().await;
    assert!(!f.journal().contains("SYNTHETIC_APPROVAL_BODY"));
}

#[tokio::test]
async fn interaction_unknown_prompt_delivery_never_activates_references_or_retries() {
    let mut f = Fixture::new(true).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, command_interaction(json!(73)))
        .await;
    f.offer_interaction(&job, &attempt).await;
    let reference = button_reference(&f.prompt(0), "Allow once");
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    f.process_callback(&callback_update(21, "SYNTHETIC_UNKNOWN_QUERY", &reference))
        .await;
    f.offer_interaction(&job, &attempt).await;
    f.no_native_answer().await;
    assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
    assert_eq!(callback_receipts(&f.journal()), 0);
    assert!(!f.journal().contains(&reference));
    assert_eq!(
        f.service.snapshot().last_task.unwrap().reason,
        Some("channel_interaction_send_unknown")
    );
}

#[tokio::test]
async fn interaction_uncertain_old_job_cannot_revoke_a_replacement_binding() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    f.admit(20, 20).await;
    let old = f.pop().active;
    f.service.revoke();
    f.bind_with_consent().await;
    f.admit(21, 21).await;
    let replacement = f.service.snapshot().binding.active.unwrap();
    assert_ne!(old.generation, replacement.generation);
    f.service.core.revoke_job(old);
    let mut wrong_target = replacement;
    wrong_target.target.thread_id = Uuid::from_u128(999);
    f.service.core.revoke_job(wrong_target);
    assert!(f.service.core.valid(replacement));
    assert_eq!(f.service.snapshot().queued, 1);
    f.service.core.revoke_job(replacement);
    assert!(f.service.snapshot().binding.active.is_none());
    assert_eq!(f.service.snapshot().queued, 0);
}

#[tokio::test]
async fn interaction_pre_ack_text_and_unknown_submission_never_queue_as_new_tasks() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    f.admit(20, 20).await;
    let job = f.pop();
    let task = job.id;
    let generation = job.active.generation;
    let core = f.service.core.clone();
    let executor = tokio::spawn(async move { execute_job(&core, &job).await });
    let start = read(&mut f.control).await;
    assert_eq!(start["method"], "turn/start");
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .active_interaction
            .is_some()
    );
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        generation,
        &update(21, 21, "SYNTHETIC_PRE_ACK_TEXT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    let client = start["params"]["clientUserMessageId"].clone();
    for value in [
        json!({"method":"turn/started","params":{"threadId":thread(),"turn":{"id":interaction_turn(),"status":"inProgress"}}}),
        json!({"method":"item/started","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
        json!({"method":"item/completed","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
        json!({"id":"pre-ack-question","method":"item/tool/requestUserInput","params":{
            "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item","questions":[{
                "id":"choice","header":"Choice","question":"SYNTHETIC_PRE_ACK_QUESTION",
                "isOther":false,"isSecret":false,"options":[{"label":"Yes","description":"Continue"},
                {"label":"No","description":"Stop"}]}]}}),
    ] {
        send(&mut f.native, value.clone()).await;
        assert_eq!(read(&mut f.tui).await, value);
    }
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        generation,
        &update(22, 22, "SYNTHETIC_PRE_ACK_QUESTION_TEXT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    assert!(
        f.bot
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.get("reply_markup").is_none())
    );
    // The unacknowledged submission may already have taken effect. Closing its
    // independent control transport cannot be treated as permission for new text.
    f.control.close(None).await.unwrap();
    bounded(executor).await.unwrap();
    let view = f.service.snapshot().last_task.unwrap();
    assert_eq!(view.id, task);
    assert_eq!(view.phase, "uncertain");
    assert!(!view.native_confirmed);
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .binding
            .snapshot(Instant::now())
            .active
            .is_none()
    );
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        generation,
        &update(23, 23, "SYNTHETIC_UNKNOWN_SUBMISSION_TEXT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().queued, 0);
    for id in [21, 22, 23] {
        assert!(
            f.service
                .core
                .writer()
                .unwrap()
                .is_rejected(
                    BOT,
                    id,
                    Some(DeliveryKey {
                        bot_id: BOT,
                        chat_id: CHAT,
                        message_id: id,
                    })
                )
                .await
                .unwrap()
        );
    }
    let journal = f.journal();
    assert!(journal.contains("submitting"));
    assert!(!journal.contains("SYNTHETIC_PRE_ACK"));
    assert!(!journal.contains("SYNTHETIC_UNKNOWN_SUBMISSION_TEXT"));
    let value = json!({"method":"synthetic/forwardingStillLive","params":{"synthetic":true}});
    send(&mut f.native, value.clone()).await;
    assert_eq!(read(&mut f.tui).await, value);
}

#[tokio::test]
async fn interaction_execute_job_loop_prompts_answers_completes_and_cleans_up() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    f.admit(20, 20).await;
    let job = f.pop();
    let core = f.service.core.clone();
    let executor = tokio::spawn(async move { execute_job(&core, &job).await });
    let start = read(&mut f.control).await;
    assert_eq!(start["method"], "turn/start");
    assert_eq!(start["params"]["input"][0]["text"], BODY);
    let client = start["params"]["clientUserMessageId"].clone();
    send(
        &mut f.control,
        json!({"id":start["id"],"result":{"turn":{"id":interaction_turn()}}}),
    )
    .await;
    for value in [
        json!({"method":"turn/started","params":{"threadId":thread(),"turn":{"id":interaction_turn(),"status":"inProgress"}}}),
        json!({"method":"item/started","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
        json!({"method":"item/completed","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"userMessage","id":"owned-user","clientId":client,"content":[]}}}),
        command_interaction(json!(73)),
    ] {
        send(&mut f.native, value.clone()).await;
        assert_eq!(read(&mut f.tui).await, value);
    }
    // Only the production execute_job loop offers this request.
    bounded(async {
        loop {
            if f.service.core.inner.lock().unwrap().interactions.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let reference = button_reference(&f.prompt(0), "Allow once");
    f.process_callback(&callback_update(21, "SYNTHETIC_EXECUTOR_QUERY", &reference))
        .await;
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":73,"result":{"decision":"accept"}})
    );
    for value in [
        json!({"method":"serverRequest/resolved","params":{"threadId":thread(),"requestId":73}}),
        json!({"method":"item/completed","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"agentMessage","id":"final","phase":"final_answer","text":FINAL}}}),
        json!({"method":"turn/completed","params":{"threadId":thread(),"turn":{"id":interaction_turn(),"status":"completed"}}}),
    ] {
        send(&mut f.native, value.clone()).await;
        assert_eq!(read(&mut f.tui).await, value);
    }
    bounded(executor).await.unwrap();
    let view = f.service.snapshot().last_task.unwrap();
    assert!(view.native_confirmed && view.native_completed);
    assert_eq!(view.delivery, "confirmed");
    {
        let inner = f.service.core.inner.lock().unwrap();
        assert!(inner.interactions.is_empty());
        assert!(inner.active_interaction.is_none());
    }
    {
        let requests = f.bot.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.get("reply_markup").is_some())
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request["text"] == FINAL)
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.get("callback_query_id").is_some())
                .count(),
            1
        );
    }
    f.process_callback(&callback_update(
        22,
        "SYNTHETIC_EXECUTOR_LATE_QUERY",
        &reference,
    ))
    .await;
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert!(!f.journal().contains("SYNTHETIC_EXECUTOR_QUERY"));
    assert!(!f.journal().contains(&reference));
}

#[tokio::test]
async fn interaction_paused_receipt_text_claim_excludes_buttons_and_rechecks_local_or_revoke() {
    for invalidation in ["none", "local", "revoke"] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        f.native_interaction(&attempt, one_question(json!(83),
            json!([{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]), true)).await;
        f.offer_interaction(&job, &attempt).await;
        let reference = button_reference(&f.prompt(0), "1.1");
        let id = *f
            .service
            .core
            .inner
            .lock()
            .unwrap()
            .interactions
            .keys()
            .next()
            .unwrap();
        let core = f.service.core.clone();
        let client = f.bot.client.clone();
        let generation = f.service.snapshot().binding.generation;
        let writer = core.writer().unwrap();
        let before = f.journal();
        let resume = bounded(writer.pause_for_test()).await.unwrap();
        let text = "SYNTHETIC_PAUSED_TEXT_REPLY";
        let reply = question_reply_update(21, 21, 77, text);
        // Enter the production handler directly to isolate its first durable
        // await from process_update's earlier dedup lookup. The real worker is
        // paused, so no timing assumption can let this receipt finish early.
        let mut replying = Box::pin(interactions::question_reply(
            &core, &client, BOT, generation, &reply, &writer,
        ));
        assert!(futures_util::poll!(replying.as_mut()).is_pending());
        assert!(core.inner.lock().unwrap().interactions.is_empty());
        assert_eq!(f.journal(), before);
        let button = callback_update(22, "SYNTHETIC_PAUSED_BUTTON_QUERY", &reference);
        let mut clicking = Box::pin(interactions::callback(
            &core, &client, BOT, generation, &button, &writer,
        ));
        assert!(futures_util::poll!(clicking.as_mut()).is_pending());
        assert!(core.inner.lock().unwrap().interactions.is_empty());
        assert_eq!(f.journal(), before);
        match invalidation {
            "local" => {
                let local = json!({"id":83,"result":{"answers":{"original-question":{"answers":["Local answer"]}}}});
                send(&mut f.tui, local.clone()).await;
                assert_eq!(read(&mut f.native).await, local);
                bounded(async {
                    while attempt.revalidate().is_err()
                        || attempt.interactions().current(id).is_ok()
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await;
            }
            "revoke" => {
                f.service.revoke();
            }
            _ => {
                assert!(attempt.interactions().current(id).is_ok());
            }
        }
        // No fsync has run since the pause. Resume both queued operations, then
        // await their actual completion before checking the native socket.
        assert_eq!(f.journal(), before);
        resume.send(()).unwrap();
        let (text_result, button_result) =
            bounded(async { tokio::join!(replying, clicking) }).await;
        assert!(text_result.is_ok() && button_result.is_ok());
        assert_eq!(question_reply_receipts(&f.journal()), 1);
        assert_eq!(callback_receipts(&f.journal()), 0);
        assert!(writer.is_rejected(BOT, 22, None).await.unwrap());
        if invalidation == "none" {
            assert_eq!(
                read(&mut f.control).await,
                json!({"id":83,"result":{"answers":{"original-question":{"answers":[text]}}}})
            );
            assert_eq!(attempt.interactions().awaiting_question(), Ok(true));
        }
        f.no_native_answer().await;
        assert!(core.inner.lock().unwrap().interactions.is_empty());
        assert_eq!(f.service.snapshot().queued, 0);
        for private in [text, "SYNTHETIC_PAUSED_BUTTON_QUERY", &reference] {
            assert!(!f.journal().contains(private));
        }
    }
}

include!("cancel_tests.rs");
