// Full Service ownership seam. Synthetic stored phases are seeded through the
// real receipt worker; they do not claim pre-crash native execution evidence.
#[cfg(target_os = "macos")]
mod restart_seam {
    use super::*;
    use tokio::sync::{mpsc, oneshot};

    struct PollOffer {
        offset: Option<i64>,
        reply: oneshot::Sender<Vec<Value>>,
    }
    #[derive(Clone, Copy, Default)]
    struct Calls {
        inspect: usize,
        polls: usize,
        sends: usize,
        acks: usize,
    }
    struct RestartBot {
        address: std::net::SocketAddr,
        polls: mpsc::Receiver<PollOffer>,
        outbound: mpsc::Receiver<Value>,
        calls: Arc<Mutex<Calls>>,
        task: Option<JoinHandle<()>>,
        stop: Option<oneshot::Sender<()>>,
    }
    impl Drop for RestartBot {
        fn drop(&mut self) {
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }
    impl RestartBot {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (poll_tx, polls) = mpsc::channel(8);
            let (out_tx, outbound) = mpsc::channel(8);
            let calls = Arc::new(Mutex::new(Calls::default()));
            let recorded = calls.clone();
            let (stop, mut stopping) = oneshot::channel();
            let task = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                let next_message = Arc::new(std::sync::atomic::AtomicI64::new(77));
                loop {
                    tokio::select! {
                        _ = &mut stopping => {
                            // Deliberately cancel outstanding long polls, then
                            // inspect every join result. Aborting an unfinished
                            // handler must not hide an already-completed panic.
                            connections.abort_all();
                            while let Some(result) = connections.join_next().await {
                                match result {
                                    Ok(()) => {},
                                    Err(error) if error.is_cancelled() => {},
                                    Err(error) => panic!("synthetic Bot handler failed: {error}"),
                                }
                            }
                            break;
                        }
                        accepted = listener.accept() => {
                            let (stream, _) = accepted.unwrap();
                            connections.spawn(Self::serve(stream, poll_tx.clone(), out_tx.clone(), recorded.clone(), next_message.clone()));
                        }
                        done = connections.join_next(), if !connections.is_empty() => {
                            // Propagate a synthetic server panic rather than silently
                            // treating a truncated request as successful evidence.
                            done.unwrap().unwrap();
                        }
                    }
                }
            });
            Self {
                address,
                polls,
                outbound,
                calls,
                task: Some(task),
                stop: Some(stop),
            }
        }
        async fn shutdown(mut self) {
            let _ = self.stop.take().unwrap().send(());
            // Success requires observing the supervisor result, including any
            // handler panic propagated before or during the shutdown drain.
            // Keep the handle owned until the wait returns: on timeout/panic,
            // Drop still aborts the supervisor and its owned JoinSet.
            let result = bounded(self.task.as_mut().unwrap()).await;
            self.task.take();
            result.unwrap();
        }
        fn snapshot(&self) -> Calls {
            *self.calls.lock().unwrap()
        }
        async fn poll(&mut self) -> PollOffer {
            bounded(async {
                loop {
                    let offer = self.polls.recv().await.unwrap();
                    if !offer.reply.is_closed() {
                        return offer;
                    }
                }
            })
            .await
        }
        async fn serve(
            mut stream: tokio::net::TcpStream,
            polls: mpsc::Sender<PollOffer>,
            outbound: mpsc::Sender<Value>,
            calls: Arc<Mutex<Calls>>,
            next_message: Arc<std::sync::atomic::AtomicI64>,
        ) {
            let mut bytes = Vec::new();
            let end = loop {
                let mut buffer = [0; 2048];
                let size = stream.read(&mut buffer).await.unwrap();
                if size == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..size]);
                assert!(bytes.len() <= 65536);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let method = header
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_owned();
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(end + length <= 65536);
            while bytes.len() < end + length {
                let mut buffer = [0; 2048];
                let size = stream.read(&mut buffer).await.unwrap();
                if size == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..size]);
                assert!(bytes.len() <= 65536);
            }
            let body: Value = serde_json::from_slice(&bytes[end..end + length]).unwrap();
            let result = match method.as_str() {
                "getMe" => {
                    calls.lock().unwrap().inspect += 1;
                    json!({"id":BOT,"is_bot":true})
                }
                "getWebhookInfo" => {
                    calls.lock().unwrap().inspect += 1;
                    json!({"url":""})
                }
                "getUpdates" => {
                    calls.lock().unwrap().polls += 1;
                    let (reply, receive) = oneshot::channel();
                    if polls
                        .send(PollOffer {
                            offset: body["offset"].as_i64(),
                            reply,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let mut closed = [0; 1];
                    // A binding-generation change cancels the HTTP future. Read
                    // EOF retires that poll offer before a new batch is supplied.
                    tokio::select! {
                        response = receive => match response { Ok(batch) => json!(batch), Err(_) => return },
                        _ = stream.read(&mut closed) => return,
                    }
                }
                "sendMessage" => {
                    calls.lock().unwrap().sends += 1;
                    if outbound.send(body).await.is_err() {
                        return;
                    }
                    let id = next_message.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    json!({"message_id":id,"chat":{"id":CHAT,"type":"private"}})
                }
                "answerCallbackQuery" => {
                    calls.lock().unwrap().acks += 1;
                    json!(true)
                }
                _ => panic!("unexpected synthetic Bot method"),
            };
            let body = json!({"ok":true,"result":result}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            // Cancellation is expected for old long polls; a closed client is
            // never treated as a successful request or a reason to retry it.
            let _ = stream.write_all(response.as_bytes()).await;
        }
    }
    struct Driver(JoinHandle<()>);
    impl Drop for Driver {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    #[tokio::test]
    async fn service_restart_requires_fresh_configuration_pairing_and_input_before_normal_execution()
     {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = root.path().canonicalize().unwrap();
        let path = parent.join("receipts");
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
        let (client, peer) = UnixStream::pair().unwrap();
        let socket = Socket::from_raw_socket(client, Role::Client, None).await;
        let mut control = Socket::from_raw_socket(peer, Role::Server, None).await;
        let (handle, driver) = ControlHandle::test_socket(socket);
        let _driver = Driver(driver);
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
        send(&mut native, json!({"id":1,"result":{"thread":{"id":thread(),"source":"cli","status":{"type":"idle"}}}})).await;
        read(&mut tui).await;
        bounded(async {
            while session.handle().qualified(thread()).is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let mut bot = RestartBot::new().await;
        let old = Service::new(path.clone());
        old.api_for_test(bot.address);
        old.register(session.handle());
        let configured = bounded(old.configure(Some("101:synthetic_restart_token".into())))
            .await
            .unwrap();
        assert!(configured.configured && configured.binding.active.is_none());
        assert_eq!(bot.snapshot().inspect, 2);
        // Public configuration and confirmation establish actual old authority.
        // The old executor is deliberately not started: production admission
        // leaves a deterministic volatile queued body for Service Drop to erase.
        let code = old.begin_pair().unwrap();
        let old_client = old.core.inner.lock().unwrap().client.clone().unwrap();
        bounded(process_update(
            &old.core,
            &old_client,
            BOT,
            code.generation,
            &update(1, 1, &code.command),
        ))
        .await
        .unwrap();
        let candidate = old.snapshot().binding.candidate.unwrap();
        let source = session.snapshot();
        old.confirm(candidate, source.run_id, thread(), source.revision, true)
            .unwrap();
        let old_active = old.snapshot().binding.active.unwrap();
        assert!(old_active.interactive_consent);
        bounded(process_update(
            &old.core,
            &old_client,
            BOT,
            old_active.generation,
            &update(10, 10, "SYNTHETIC_PRE_RESTART_QUEUED_BODY"),
        ))
        .await
        .unwrap();
        assert_eq!(old.snapshot().queued, 1);
        assert_eq!(old.snapshot().binding.active, Some(old_active));
        drop(old_client);
        let writer = old.core.writer().unwrap();
        // Submitting/Closed and the callback below are legal synthetic stored
        // metadata, not claims about pre-crash native execution or completion.
        for (id, task) in [(11, 111), (12, 112)] {
            assert_eq!(
                writer
                    .admit(
                        DeliveryKey {
                            bot_id: BOT,
                            chat_id: CHAT,
                            message_id: id
                        },
                        id,
                        Uuid::from_u128(task)
                    )
                    .await
                    .unwrap(),
                Admission::Fresh
            );
        }
        writer
            .mark_submitting(Uuid::from_u128(111), Uuid::from_u128(211))
            .await
            .unwrap();
        writer.close(Uuid::from_u128(112)).await.unwrap();
        writer
            .admit_callback(
                BOT,
                13,
                "SYNTHETIC_PRE_RESTART_QUERY".into(),
                Uuid::from_u128(111),
                Uuid::from_u128(311),
            )
            .await
            .unwrap();
        let expected = Counts {
            received: 1,
            submitting: 1,
            closed: 1,
        };
        assert_eq!(writer.load().await.unwrap().recovered, expected);
        let credential = fs::read(path.join("telegram.token")).unwrap();
        let journal_before = fs::read(path.join("receipts.jsonl")).unwrap();
        drop(old); // Real Service Drop owns shutdown; do not take its worker.
        bounded(async {
            loop {
                match super::super::super::store::Store::open(&path) {
                    Ok(store) => {
                        drop(store);
                        break;
                    }
                    Err(super::super::super::store::Failure::Busy) => {
                        tokio::time::sleep(Duration::from_millis(5)).await
                    }
                    Err(_) => panic!("synthetic receipt reopen failed"),
                }
            }
        })
        .await;
        assert!(matches!(
            writer.load().await,
            Err(receipts::Failure::Stopped)
        ));
        drop(writer);
        let service = Service::new(path.clone());
        service.api_for_test(bot.address);
        service.register(session.handle());
        service.start();
        let cold = service.snapshot();
        assert!(!cold.configured && cold.binding.active.is_none() && cold.queued == 0);
        assert_eq!(cold.recovered, Counts::default());
        assert_eq!(fs::read(path.join("telegram.token")).unwrap(), credential);
        assert_eq!(
            fs::read(path.join("receipts.jsonl")).unwrap(),
            journal_before
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(30), control.next())
                .await
                .is_err()
        );
        assert_eq!(
            bot.snapshot().polls + bot.snapshot().sends + bot.snapshot().acks,
            0
        );
        let loaded = bounded(service.configure(None)).await.unwrap();
        assert!(loaded.configured && loaded.binding.active.is_none() && loaded.queued == 0);
        assert_eq!(loaded.recovered, expected);
        assert_eq!(bot.snapshot().inspect, 4);
        assert_eq!(fs::read(path.join("telegram.token")).unwrap(), credential);
        let mut inactive_poll = bot.poll().await;
        assert_eq!(inactive_poll.offset, None);
        let code = service.begin_pair().unwrap();
        bounded(inactive_poll.reply.closed()).await;
        let pairing_poll = bot.poll().await;
        pairing_poll
            .reply
            .send(vec![update(1000, 1000, &code.command)])
            .unwrap();
        let candidate = bounded(async {
            loop {
                if let Some(candidate) = service.snapshot().binding.candidate {
                    break candidate;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let mut paired_poll = bot.poll().await;
        assert_eq!(paired_poll.offset, Some(1001));
        let source = session.snapshot();
        let active = service
            .confirm(candidate, source.run_id, thread(), source.revision, true)
            .unwrap();
        assert!(active.binding.active.unwrap().interactive_consent);
        bounded(paired_poll.reply.closed()).await;
        let fresh_poll = bot.poll().await;
        let client = service.core.inner.lock().unwrap().client.clone().unwrap();
        let generation = service.snapshot().binding.generation;
        let before = fs::read(path.join("receipts.jsonl")).unwrap();
        // Old task message identity must not learn the otherwise-fresh update
        // alias. Normal polling below then admits that update with a fresh key.
        bounded(process_update(
            &service.core,
            &client,
            BOT,
            generation,
            &update(1003, 10, "SYNTHETIC_OLD_MESSAGE"),
        ))
        .await
        .unwrap();
        assert_eq!(service.snapshot().queued, 0);
        assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), control.next())
                .await
                .is_err()
        );
        fresh_poll
            .reply
            .send(vec![update(1003, 1003, BODY)])
            .unwrap();
        let request = read(&mut control).await;
        assert_eq!(request["method"], "turn/start");
        assert_eq!(request["params"]["threadId"], thread().to_string());
        assert_eq!(request["params"]["input"][0]["text"], BODY);
        let client_id = request["params"]["clientUserMessageId"].clone();
        let turn = interaction_turn();
        send(
            &mut control,
            json!({"id":request["id"],"result":{"turn":{"id":turn}}}),
        )
        .await;
        for value in [
            json!({"method":"turn/started","params":{"threadId":thread(),"turn":{"id":turn,"status":"inProgress"}}}),
            json!({"method":"item/started","params":{"threadId":thread(),"turnId":turn,"item":{"type":"userMessage","id":"restart-user","clientId":client_id,"content":[]}}}),
            json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"userMessage","id":"restart-user","clientId":client_id,"content":[]}}}),
            command_interaction(json!("restart-command-rpc")),
        ] {
            send(&mut native, value.clone()).await;
            assert_eq!(read(&mut tui).await, value);
        }
        let prompt = bounded(bot.outbound.recv()).await.unwrap();
        assert!(
            prompt["text"]
                .as_str()
                .unwrap()
                .contains("SYNTHETIC_APPROVAL_BODY")
        );
        let reference = button_reference(&prompt, "Allow once");
        bounded(async {
            while service.core.inner.lock().unwrap().interactions.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let pending: Vec<_> = service
            .core
            .inner
            .lock()
            .unwrap()
            .interactions
            .keys()
            .copied()
            .collect();
        let before = fs::read(path.join("receipts.jsonl")).unwrap();
        bounded(process_update(
            &service.core,
            &client,
            BOT,
            generation,
            &callback_update(1004, "SYNTHETIC_PRE_RESTART_QUERY", &reference),
        ))
        .await
        .unwrap();
        let after: Vec<_> = service
            .core
            .inner
            .lock()
            .unwrap()
            .interactions
            .keys()
            .copied()
            .collect();
        assert_eq!(after, pending);
        assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), control.next())
                .await
                .is_err()
        );
        let answer_poll = bot.poll().await;
        assert_eq!(answer_poll.offset, Some(1004));
        answer_poll
            .reply
            .send(vec![callback_update(
                1004,
                "SYNTHETIC_POST_RESTART_QUERY",
                &reference,
            )])
            .unwrap();
        assert_eq!(
            read(&mut control).await,
            json!({"id":"restart-command-rpc","result":{"decision":"accept"}})
        );
        bounded(async {
            while bot.snapshot().acks != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        for value in [
            json!({"method":"serverRequest/resolved","params":{"threadId":thread(),"requestId":"restart-command-rpc"}}),
            json!({"method":"item/completed","params":{"threadId":thread(),"turnId":turn,"item":{"type":"agentMessage","id":"restart-final","phase":"final_answer","text":FINAL}}}),
        ] {
            send(&mut native, value.clone()).await;
            assert_eq!(read(&mut tui).await, value);
        }
        // Qualification checks the capture watermark, so the final item has
        // reached the decoder before checking that it is still withheld.
        bounded(async {
            while session.handle().qualified(thread()).is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), bot.outbound.recv())
                .await
                .is_err()
        );
        assert_eq!(bot.snapshot().sends, 1);
        let before_completion = service.snapshot().last_task.unwrap();
        assert!(!before_completion.native_completed);
        let completed = json!({"method":"turn/completed","params":{"threadId":thread(),"turn":{"id":turn,"status":"completed"}}});
        send(&mut native, completed.clone()).await;
        assert_eq!(read(&mut tui).await, completed);
        let final_message = bounded(bot.outbound.recv()).await.unwrap();
        assert_eq!(final_message["text"], FINAL);
        let writer = service.core.writer().unwrap();
        bounded(async {
            while writer.load().await.unwrap().recovered
                != (Counts {
                    received: 1,
                    submitting: 1,
                    closed: 2,
                })
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let last = service.snapshot().last_task.unwrap();
        assert!(last.native_completed && last.delivery == "confirmed");
        assert_eq!(bot.snapshot().sends, 2);
        assert_eq!(bot.snapshot().acks, 1);
        assert_eq!(service.snapshot().queued, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), control.next())
                .await
                .is_err()
        );
        assert!(matches!(
            bot.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(fs::read(path.join("telegram.token")).unwrap(), credential);
        let journal = fs::read_to_string(path.join("receipts.jsonl")).unwrap();
        assert_eq!(callback_receipts(&journal), 2);
        let events: Vec<Value> = journal
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["record"]["event"].clone())
            .collect();
        let received = |update_id: i64| {
            let matching: Vec<_> = events
                .iter()
                .filter(|event| event["kind"] == "received" && event["update_id"] == update_id)
                .collect();
            assert_eq!(matching.len(), 1);
            Uuid::parse_str(matching[0]["task"].as_str().unwrap()).unwrap()
        };
        let fresh_task = received(1003);
        let old_queued_task = received(10);
        assert_ne!(fresh_task, old_queued_task);
        assert_eq!(last.id, fresh_task);
        let lifecycle = |task: Uuid| -> Vec<&str> {
            events
                .iter()
                .filter(|event| event["task"] == task.to_string())
                .filter_map(|event| event["kind"].as_str())
                .filter(|kind| {
                    matches!(
                        *kind,
                        "received" | "submitting" | "closed" | "queued_canceled"
                    )
                })
                .collect()
        };
        assert_eq!(lifecycle(fresh_task), ["received", "submitting", "closed"]);
        assert_eq!(lifecycle(old_queued_task), ["received"]);
        assert_eq!(lifecycle(Uuid::from_u128(111)), ["received", "submitting"]);
        assert_eq!(lifecycle(Uuid::from_u128(112)), ["received", "closed"]);
        let intent = events
            .iter()
            .find(|event| event["task"] == fresh_task.to_string() && event["kind"] == "submitting")
            .unwrap();
        assert_eq!(intent["client_id"], client_id);
        for private in [
            BODY,
            FINAL,
            "SYNTHETIC_OLD_MESSAGE",
            "SYNTHETIC_PRE_RESTART_QUEUED_BODY",
            "SYNTHETIC_PRE_RESTART_QUERY",
            "SYNTHETIC_POST_RESTART_QUERY",
            "SYNTHETIC_APPROVAL_BODY",
            "restart-command-rpc",
            "101:synthetic_restart_token",
            &reference,
        ] {
            assert!(!journal.contains(private));
        }
        drop(client);
        drop(writer);
        drop(service);
        bot.shutdown().await;
        drop(relay);
    }
}
