// Queued cancellation tests share the isolated synthetic transport Fixture.
async fn queued_cancel_notice(f: &Fixture, job: &Job) -> String {
    cancellations::request(&f.service.core, job);
    let outbound = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .cancel_outbox
        .pop_front()
        .unwrap();
    bounded(cancellations::deliver(&f.service.core, outbound)).await;
    let requests = f.bot.requests.lock().unwrap();
    let notice = requests.last().unwrap();
    assert_eq!(notice["chat_id"], CHAT);
    assert!(notice.get("parse_mode").is_none());
    assert!(notice.get("reply_markup").is_none());
    let command = notice["text"]
        .as_str()
        .unwrap()
        .lines()
        .find(|line| line.starts_with("/cancel "))
        .unwrap();
    let reference = command.strip_prefix("/cancel ").unwrap();
    assert_eq!(reference.len(), 32);
    assert!(
        reference
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    command.to_owned()
}
async fn queued_cancel_update(f: &Fixture, update_id: i64, message_id: i64, command: &str) {
    bounded(process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        f.service.snapshot().binding.generation,
        &update(update_id, message_id, command),
    ))
    .await
    .unwrap();
}
fn queued_cancel_events(f: &Fixture, task: Uuid) -> Vec<Value> {
    f.journal()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["record"]["event"].clone())
        .filter(|event| event["task"].as_str() == Some(task.to_string().as_str()))
        .collect()
}
async fn queued_cancel_assert_closed(f: &Fixture, task: Uuid) {
    let events = queued_cancel_events(f, task);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["kind"], "received");
    assert_eq!(events[1]["kind"], "queued_canceled");
    let writer = f.service.core.writer().unwrap();
    assert_eq!(
        writer.mark_submitting(task, Uuid::new_v4()).await,
        Err(receipts::Failure::Store(
            super::super::store::Failure::InvalidTransition
        ))
    );
}

#[tokio::test]
async fn queued_cancel_removes_queued_task_and_sends_only_static_post_fsync_ack() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    let task = job.id;
    let command = queued_cancel_notice(&f, &job).await;
    f.service
        .core
        .update(&job, "waiting", false, false, "pending", None);
    f.service.core.inner.lock().unwrap().queue.push_back(job);
    assert_eq!(f.service.snapshot().queued, 1);
    queued_cancel_update(&f, 20, 20, &command).await;
    assert_eq!(f.service.snapshot().queued, 0);
    queued_cancel_assert_closed(&f, task).await;
    let view = f.service.snapshot().last_task.unwrap();
    assert_eq!(view.phase, "canceled");
    assert!(!view.native_confirmed && !view.native_completed);
    assert_eq!(view.delivery, "notSubmitted");
    let ack = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .cancel_outbox
        .pop_front()
        .unwrap();
    bounded(cancellations::deliver(&f.service.core, ack)).await;
    {
        let requests = f.bot.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]["text"],
            "Queued task canceled before native submission."
        );
        assert_eq!(requests[1]["chat_id"], CHAT);
    }
    assert_eq!(
        f.service.snapshot().last_task.unwrap().delivery,
        "confirmed"
    );
    let before = f.journal();
    queued_cancel_update(&f, 20, 20, &command).await;
    queued_cancel_update(&f, 21, 20, &command).await;
    assert_eq!(f.journal(), before);
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_outbox
            .is_empty()
    );
    assert!(!before.contains(&command));
    assert!(!before.contains(BODY));
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_stops_popped_busy_waiting_job_without_independent_close_or_native_rpc() {
    let mut f = Fixture::new(false).await;
    send(&mut f.native, json!({"method":"thread/status/changed","params":{"threadId":thread(),"status":{"type":"active","activeFlags":[]}}})).await;
    read(&mut f.tui).await;
    bounded(async {
        while !f
            .session
            .snapshot()
            .targets
            .iter()
            .any(|target| target.thread_id == thread() && target.busy)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    f.admit(2, 2).await;
    let job = f.pop();
    let core = f.service.core.clone();
    let mut executing = Box::pin(execute_job(&core, &job));
    assert!(futures_util::poll!(executing.as_mut()).is_pending());
    let command = queued_cancel_notice(&f, &job).await;
    queued_cancel_update(&f, 20, 20, &command).await;
    assert!(bounded(executing).await == cancellations::ReceiptEnd::OwnedByCancellation);
    queued_cancel_assert_closed(&f, job.id).await;
    assert_eq!(f.service.snapshot().last_task.unwrap().phase, "canceled");
    for phase in ["waiting", "abandoned"] {
        core.update(
            &job,
            phase,
            false,
            false,
            "withheld",
            Some("channel_binding_changed"),
        );
        let view = f.service.snapshot().last_task.unwrap();
        assert_eq!(view.phase, "canceled");
        assert_eq!(view.delivery, "notSubmitted");
        assert_eq!(view.reason, Some("channel_queue_canceled"));
    }
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_known_input_does_not_consume_another_valid_task_reference() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let first = f.pop();
    let first_command = queued_cancel_notice(&f, &first).await;
    f.admit(3, 3).await;
    let second = f.pop();
    let second_command = queued_cancel_notice(&f, &second).await;
    queued_cancel_update(&f, 20, 20, &first_command).await;
    let writer = f.service.core.writer().unwrap();
    writer
        .reject(
            BOT,
            30,
            Some(DeliveryKey {
                bot_id: BOT,
                chat_id: CHAT,
                message_id: 30,
            }),
        )
        .await
        .unwrap();
    let before = f.journal();
    for (update_id, message_id) in [(20, 21), (21, 20), (2, 22), (22, 2), (30, 31), (31, 30)] {
        queued_cancel_update(&f, update_id, message_id, &second_command).await;
        assert!(second.dispatch.open());
        assert!(
            f.service
                .core
                .inner
                .lock()
                .unwrap()
                .cancel_offers
                .contains_key(&second.id)
        );
        assert_eq!(f.journal(), before);
    }
    assert_eq!(
        writer.load().await.unwrap().recovered,
        Counts {
            received: 1,
            submitting: 0,
            closed: 1
        }
    );
    queued_cancel_update(&f, 21, 21, &second_command).await;
    queued_cancel_assert_closed(&f, second.id).await;
    assert_eq!(
        writer.load().await.unwrap().recovered,
        Counts {
            received: 0,
            submitting: 0,
            closed: 2
        }
    );
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_requires_direct_exact_authorized_input_and_keeps_reference_on_rejection() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    let command = queued_cancel_notice(&f, &job).await;
    let generation = job.active.generation;
    let mut invalid = Vec::new();
    for field in [
        "forward_origin",
        "is_automatic_forward",
        "edit_date",
        "via_bot",
        "reply_to_message",
        "external_reply",
        "quote",
    ] {
        let mut value = update(20, 20, &command);
        value["message"][field] = json!({});
        invalid.push((BOT, generation, value));
    }
    let mut wrong_user = update(20, 20, &command);
    wrong_user["message"]["from"]["id"] = json!(CHAT + 1);
    invalid.push((BOT, generation, wrong_user));
    let mut wrong_chat = update(20, 20, &command);
    wrong_chat["message"]["chat"]["id"] = json!(CHAT + 1);
    invalid.push((BOT, generation, wrong_chat));
    invalid.push((BOT + 1, generation, update(20, 20, &command)));
    invalid.push((BOT, Uuid::new_v4(), update(20, 20, &command)));
    for text in [
        format!("{command} "),
        command.to_ascii_uppercase(),
        format!("/cancel@bot {}", command.trim_start_matches("/cancel ")),
        "/cancel 00000000000000000000000000000000".to_owned(),
    ] {
        invalid.push((BOT, generation, update(20, 20, &text)));
    }
    for (index, (bot, generation, mut value)) in invalid.into_iter().enumerate() {
        let id = index as i64 + 20;
        value["update_id"] = json!(id);
        value["message"]["message_id"] = json!(id);
        bounded(process_update(
            &f.service.core,
            &f.bot.client,
            bot,
            generation,
            &value,
        ))
        .await
        .unwrap();
        assert!(job.dispatch.open());
        assert!(
            f.service
                .core
                .inner
                .lock()
                .unwrap()
                .cancel_offers
                .contains_key(&job.id)
        );
        assert_eq!(queued_cancel_events(&f, job.id).len(), 1);
    }
    queued_cancel_update(&f, 100, 100, &command).await;
    queued_cancel_assert_closed(&f, job.id).await;
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_submit_claim_wins_before_receipt_intent_and_retires_reference() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    let command = queued_cancel_notice(&f, &job).await;
    assert!(cancellations::claim(&f.service.core, &job));
    // No Submitting write yet: local winner is chosen before the first await.
    assert_eq!(queued_cancel_events(&f, job.id).len(), 1);
    queued_cancel_update(&f, 20, 20, &command).await;
    assert_eq!(queued_cancel_events(&f, job.id).len(), 1);
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_outbox
            .is_empty()
    );
    let writer = f.service.core.writer().unwrap();
    writer
        .mark_submitting(job.id, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(writer.load().await.unwrap().recovered.submitting, 1);
    assert!(cancellations::finish(&f.service.core, &job) == cancellations::ReceiptEnd::Close);
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_reserved_before_atomic_write_prevents_executor_close_even_after_revoke() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    let command = queued_cancel_notice(&f, &job).await;
    let core = f.service.core.clone();
    let writer = core.writer().unwrap();
    let before = f.journal();
    // Gate one makes the query pending deterministically. Gate two is queued
    // behind the query, so its result is ready while the atomic write is paused.
    let resume_query = bounded(writer.pause_for_test()).await.unwrap();
    let input = update(20, 20, &command);
    let mut cancel = Box::pin(cancellations::cancel(
        &core,
        BOT,
        job.active.generation,
        &input,
        &writer,
    ));
    assert!(futures_util::poll!(cancel.as_mut()).is_pending());
    let mut pause_write = Box::pin(writer.pause_for_test());
    assert!(futures_util::poll!(pause_write.as_mut()).is_pending());
    resume_query.send(()).unwrap();
    let resume_write = bounded(pause_write).await.unwrap();
    assert!(futures_util::poll!(cancel.as_mut()).is_pending());
    assert!(!job.dispatch.open());
    assert!(core.inner.lock().unwrap().cancel_offers.is_empty());
    assert_eq!(f.journal(), before);
    assert!(
        bounded(execute_job(&core, &job)).await == cancellations::ReceiptEnd::OwnedByCancellation
    );
    assert_eq!(f.journal(), before);
    f.service.revoke();
    resume_write.send(()).unwrap();
    bounded(cancel).await.unwrap();
    queued_cancel_assert_closed(&f, job.id).await;
    assert!(core.inner.lock().unwrap().cancel_outbox.is_empty());
    assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_unknown_notice_ack_grants_no_authority_and_is_not_retried() {
    let mut f = Fixture::new(true).await;
    f.admit(2, 2).await;
    let job = f.pop();
    let command = queued_cancel_notice(&f, &job).await;
    assert!(job.dispatch.open());
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_offers
            .is_empty()
    );
    cancellations::request(&f.service.core, &job);
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_outbox
            .is_empty()
    );
    queued_cancel_update(&f, 20, 20, &command).await;
    assert!(job.dispatch.open());
    assert_eq!(queued_cancel_events(&f, job.id).len(), 1);
    assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_late_notice_after_submit_claim_cannot_publish_authority() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let job = f.pop();
    cancellations::request(&f.service.core, &job);
    let notice = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .cancel_outbox
        .pop_front()
        .unwrap();
    assert!(cancellations::claim(&f.service.core, &job));
    bounded(cancellations::deliver(&f.service.core, notice)).await;
    assert!(f.bot.requests.lock().unwrap().is_empty());
    assert!(
        f.service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_offers
            .is_empty()
    );
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_receipt_end_is_per_job_when_uncertain_task_view_was_overwritten() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let unknown = f.pop();
    f.admit(3, 3).await;
    let newer = f.pop();
    let core = f.service.core.clone();
    assert!(cancellations::claim(&core, &unknown));
    let writer = core.writer().unwrap();
    writer
        .mark_submitting(unknown.id, Uuid::new_v4())
        .await
        .unwrap();
    cancellations::uncertain(&core, &unknown);
    core.update(&newer, "completed", true, true, "confirmed", None);
    assert_eq!(f.service.snapshot().last_task.unwrap().id, newer.id);
    assert!(
        bounded(execute_job(&core, &unknown)).await == cancellations::ReceiptEnd::KeepUncertain
    );
    assert_eq!(f.service.snapshot().last_task.unwrap().id, newer.id);
    let events = queued_cancel_events(&f, unknown.id);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["kind"], "submitting");
    assert_eq!(
        writer.load().await.unwrap().recovered,
        Counts {
            received: 1,
            submitting: 1,
            closed: 0
        }
    );
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_notice_authority_and_outbox_are_bounded_without_receipt_eviction() {
    let f = Fixture::new(false).await;
    let mut jobs = Vec::new();
    for id in 2..=(QUEUE_LIMIT as i64 + 3) {
        f.admit(id, id).await;
        let job = f.pop();
        cancellations::request(&f.service.core, &job);
        cancellations::request(&f.service.core, &job);
        jobs.push(job);
    }
    {
        let inner = f.service.core.inner.lock().unwrap();
        assert_eq!(inner.cancel_offers.len(), QUEUE_LIMIT + 1);
        assert_eq!(inner.cancel_outbox.len(), QUEUE_LIMIT + 1);
        assert!(!inner.cancel_offers.contains_key(&jobs.last().unwrap().id));
    }
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
        jobs.len()
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queued_cancel_inflight_duplicate_preserves_other_reference_without_learning_message() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let first = f.pop();
    let first_command = queued_cancel_notice(&f, &first).await;
    f.admit(3, 3).await;
    let second = f.pop();
    let second_command = queued_cancel_notice(&f, &second).await;
    let core = f.service.core.clone();
    let writer = core.writer().unwrap();
    let first_input = update(20, 20, &first_command);
    let conflicting = update(20, 21, &second_command);
    let resume_queries = bounded(writer.pause_for_test()).await.unwrap();
    let mut first_cancel = Box::pin(cancellations::cancel(
        &core,
        BOT,
        first.active.generation,
        &first_input,
        &writer,
    ));
    let mut second_cancel = Box::pin(cancellations::cancel(
        &core,
        BOT,
        second.active.generation,
        &conflicting,
        &writer,
    ));
    assert!(futures_util::poll!(first_cancel.as_mut()).is_pending());
    assert!(futures_util::poll!(second_cancel.as_mut()).is_pending());
    let mut pause_write = Box::pin(writer.pause_for_test());
    assert!(futures_util::poll!(pause_write.as_mut()).is_pending());
    resume_queries.send(()).unwrap();
    let resume_write = bounded(pause_write).await.unwrap();
    let before = f.journal();
    assert!(futures_util::poll!(first_cancel.as_mut()).is_pending());
    assert!(matches!(
        futures_util::poll!(second_cancel.as_mut()),
        std::task::Poll::Ready(Ok(()))
    ));
    assert!(!first.dispatch.open());
    assert!(second.dispatch.open());
    assert!(
        core.inner
            .lock()
            .unwrap()
            .cancel_offers
            .contains_key(&second.id)
    );
    assert_eq!(f.journal(), before);
    resume_write.send(()).unwrap();
    bounded(first_cancel).await.unwrap();
    queued_cancel_update(&f, 21, 21, &second_command).await;
    queued_cancel_assert_closed(&f, first.id).await;
    queued_cancel_assert_closed(&f, second.id).await;
    f.no_native_answer().await;
}

// An independently gated synthetic HTTP response tests the validity check after
// network completion, rather than only dequeuing an already-stale notice.
async fn queued_cancel_delayed_bot() -> (
    Bot,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client =
        Arc::new(Client::local(listener.local_addr().unwrap(), Duration::from_secs(3)).unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (resume, resumed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let end = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
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
        assert!(length < 65536);
        while bytes.len() < end + length {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
        recorded
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&bytes[end..end + length]).unwrap());
        entered.send(()).unwrap();
        resumed.await.unwrap();
        let body =
            json!({"ok":true,"result":{"message_id":77,"chat":{"id":CHAT,"type":"private"}}})
                .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    (
        Bot {
            client,
            requests,
            task,
        },
        waiting,
        resume,
    )
}

#[tokio::test]
async fn queued_cancel_late_http_ack_after_claim_or_revoke_never_restores_authority() {
    for revoke in [false, true] {
        let mut f = Fixture::new(false).await;
        let (bot, waiting, resume) = queued_cancel_delayed_bot().await;
        f.service.core.inner.lock().unwrap().client = Some(bot.client.clone());
        f.bot = bot;
        f.admit(2, 2).await;
        let job = f.pop();
        cancellations::request(&f.service.core, &job);
        let outbound = f
            .service
            .core
            .inner
            .lock()
            .unwrap()
            .cancel_outbox
            .pop_front()
            .unwrap();
        let core = f.service.core.clone();
        let delivery = tokio::spawn(async move { cancellations::deliver(&core, outbound).await });
        bounded(waiting).await.unwrap();
        let command = f.bot.requests.lock().unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .lines()
            .find(|line| line.starts_with("/cancel "))
            .unwrap()
            .to_owned();
        if revoke {
            f.service.revoke();
        } else {
            assert!(cancellations::claim(&f.service.core, &job));
        }
        resume.send(()).unwrap();
        bounded(delivery).await.unwrap();
        assert!(
            f.service
                .core
                .inner
                .lock()
                .unwrap()
                .cancel_offers
                .is_empty()
        );
        assert!(
            f.service
                .core
                .inner
                .lock()
                .unwrap()
                .cancel_outbox
                .is_empty()
        );
        queued_cancel_update(&f, 20, 20, &command).await;
        assert_eq!(queued_cancel_events(&f, job.id).len(), 1);
        assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
        f.no_native_answer().await;
    }
}

#[tokio::test]
async fn queued_cancel_same_reference_pending_dedup_survives_revoke_and_protects_new_binding() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let first = f.pop();
    let command = queued_cancel_notice(&f, &first).await;
    let core = f.service.core.clone();
    let writer = core.writer().unwrap();
    let input = update(20, 20, &command);
    let before = f.journal();
    let resume_query = bounded(writer.pause_for_test()).await.unwrap();
    let mut cancel = Box::pin(cancellations::cancel(
        &core,
        BOT,
        first.active.generation,
        &input,
        &writer,
    ));
    assert!(futures_util::poll!(cancel.as_mut()).is_pending());
    let mut pause_write = Box::pin(writer.pause_for_test());
    assert!(futures_util::poll!(pause_write.as_mut()).is_pending());
    resume_query.send(()).unwrap();
    let resume_write = bounded(pause_write).await.unwrap();
    assert!(futures_util::poll!(cancel.as_mut()).is_pending());
    assert!(!first.dispatch.open());
    assert!(core.inner.lock().unwrap().cancel_offers.is_empty());
    assert!(
        core.inner
            .lock()
            .unwrap()
            .cancel_pending
            .contains_key(&first.id)
    );
    // The reference is already removed. A duplicate must return before trying
    // to persist Rejected, which would compete with this still-paused cancel.
    for duplicate in [
        input.clone(),
        update(20, 21, &command),
        update(21, 20, &command),
    ] {
        let mut repeated = Box::pin(cancellations::cancel(
            &core,
            BOT,
            first.active.generation,
            &duplicate,
            &writer,
        ));
        assert!(matches!(
            futures_util::poll!(repeated.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
    }
    assert_eq!(f.journal(), before);
    f.service.revoke();
    assert!(
        core.inner
            .lock()
            .unwrap()
            .cancel_pending
            .contains_key(&first.id)
    );
    resume_write.send(()).unwrap();
    // Drain the atomic write, but leave its original caller unpolled. Local
    // re-pairing can now use the writer while the volatile reservation remains.
    assert_eq!(bounded(writer.load()).await.unwrap().recovered.closed, 1);
    f.bind().await;
    assert_ne!(
        f.service.snapshot().binding.generation,
        first.active.generation
    );
    f.admit(3, 3).await;
    let replacement = f.pop();
    let replacement_command = queued_cancel_notice(&f, &replacement).await;
    let after_rebind = f.journal();
    let resume_checks = bounded(writer.pause_for_test()).await.unwrap();
    for duplicate in [
        update(20, 21, &replacement_command),
        update(21, 20, &replacement_command),
    ] {
        let mut repeated = Box::pin(process_update(
            &core,
            &f.bot.client,
            BOT,
            replacement.active.generation,
            &duplicate,
        ));
        assert!(matches!(
            futures_util::poll!(repeated.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        assert!(replacement.dispatch.open());
        assert!(
            core.inner
                .lock()
                .unwrap()
                .cancel_offers
                .contains_key(&replacement.id)
        );
    }
    assert_eq!(f.journal(), after_rebind);
    resume_checks.send(()).unwrap();
    bounded(cancel).await.unwrap();
    assert!(
        !core
            .inner
            .lock()
            .unwrap()
            .cancel_pending
            .contains_key(&first.id)
    );
    queued_cancel_assert_closed(&f, first.id).await;
    // Neither conflicting duplicate learned identity 21, so it is still fresh.
    queued_cancel_update(&f, 21, 21, &replacement_command).await;
    queued_cancel_assert_closed(&f, replacement.id).await;
    assert_eq!(
        writer.load().await.unwrap().recovered,
        Counts {
            received: 0,
            submitting: 0,
            closed: 2
        }
    );
    f.no_native_answer().await;
}

#[tokio::test]
async fn queued_cancel_is_offered_while_old_actor_awaits_final_send_after_native_idle() {
    let mut f = Fixture::new(false).await;
    f.admit(2, 2).await;
    let old = f.pop();
    let core = f.service.core.clone();
    let writer = core.writer().unwrap();
    assert!(cancellations::claim(&core, &old));
    writer
        .mark_submitting(old.id, Uuid::new_v4())
        .await
        .unwrap();
    // Model the production actor after native completion but before its slow
    // final Bot send/receipt cleanup returns. No queued predecessor or native
    // busy flag can signal that this first new task will have to wait.
    assert_eq!(old.session.ready(old.active.target), Ok(true));
    {
        let mut inner = core.inner.lock().unwrap();
        assert!(inner.queue.is_empty());
        inner.executing = Some(old.id);
    }
    core.update(&old, "completed", true, true, "pending", None);
    f.admit(3, 3).await;
    let new_task = {
        let inner = core.inner.lock().unwrap();
        assert_eq!(inner.queue.len(), 1);
        let task = inner.queue.front().unwrap().id;
        // Assert before the helper's idempotent request: admission itself must
        // have published this notice while the production actor is occupied.
        assert_eq!(inner.cancel_outbox.len(), 1);
        assert_eq!(inner.cancel_offers.len(), 1);
        assert!(inner.cancel_offers.contains_key(&task));
        assert!(!inner.cancel_offers.contains_key(&old.id));
        task
    };
    let waiting = f.pop();
    let command = queued_cancel_notice(&f, &waiting).await;
    core.inner.lock().unwrap().queue.push_back(waiting);
    queued_cancel_update(&f, 20, 20, &command).await;
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(core.inner.lock().unwrap().executing, Some(old.id));
    queued_cancel_assert_closed(&f, new_task).await;
    let old_events = queued_cancel_events(&f, old.id);
    assert_eq!(old_events.len(), 2);
    assert_eq!(old_events[0]["kind"], "received");
    assert_eq!(old_events[1]["kind"], "submitting");
    assert_eq!(f.service.snapshot().last_task.unwrap().id, old.id);
    assert_eq!(
        writer.load().await.unwrap().recovered,
        Counts {
            received: 0,
            submitting: 1,
            closed: 1
        }
    );
    f.no_native_answer().await;
}
