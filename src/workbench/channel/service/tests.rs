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
                let body = json!({"ok":true,"result":{"message_id":77,"chat":{"id":CHAT,"type":"private"}}}).to_string();
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
            .confirm(candidate, source.run_id, thread(), source.revision)
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
        f.service
            .confirm(candidate, old_source.run_id, thread(), old_source.revision),
        Err("channel_source_changed")
    ));
    let waiting = f.service.snapshot();
    assert_eq!(waiting.binding.phase, Phase::AwaitingConfirmation);
    assert!(waiting.binding.active.is_none());
    let confirmed = f
        .service
        .confirm(candidate, current.run_id, thread(), current.revision)
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
        .confirm(candidate, source.run_id, thread(), source.revision)
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
