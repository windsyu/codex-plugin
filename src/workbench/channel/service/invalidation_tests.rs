// Real Relay/Session/Service seams; all peers, identities and bodies are synthetic.
async fn p2_invalidation_start_interaction(
    f: &mut Fixture,
) -> (Uuid, ActiveBinding, JoinHandle<cancellations::ReceiptEnd>) {
    f.admit(20, 20).await;
    let job = f.pop();
    let task = job.id;
    let active = job.active;
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
    (task, active, executor)
}

async fn p2_invalidation_forward_late_final(f: &mut Fixture) {
    for value in [
        json!({"method":"item/completed","params":{"threadId":thread(),"turnId":interaction_turn(),"item":{"type":"agentMessage","id":"late-final","phase":"final_answer","text":FINAL}}}),
        json!({"method":"turn/completed","params":{"threadId":thread(),"turn":{"id":interaction_turn(),"status":"completed"}}}),
    ] {
        send(&mut f.native, value.clone()).await;
        assert_eq!(read(&mut f.tui).await, value);
    }
}

#[tokio::test]
async fn p2_invalidation_unregister_abandons_waiting_and_queued_work_without_harming_rebind() {
    let mut f = Fixture::new(false).await;
    let busy = json!({"method":"thread/status/changed","params":{"threadId":thread(),"status":{"type":"active","activeFlags":[]}}});
    send(&mut f.native, busy.clone()).await;
    assert_eq!(read(&mut f.tui).await, busy);
    bounded(async {
        while f
            .session
            .handle()
            .ready(f.service.snapshot().binding.active.unwrap().target)
            != Ok(false)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    f.admit(20, 20).await;
    f.admit(21, 21).await;
    let active = f.service.snapshot().binding.active.unwrap();
    let actor = tokio::spawn(execute(f.service.core.clone()));
    f.service.tasks.lock().unwrap().push(actor);
    bounded(async {
        loop {
            let snapshot = f.service.snapshot();
            if snapshot.queued == 1
                && snapshot
                    .last_task
                    .is_some_and(|task| task.phase == "waiting")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    f.service.unregister(active.target.run_epoch);
    bounded(async {
        while f
            .service
            .core
            .writer()
            .unwrap()
            .load()
            .await
            .unwrap()
            .recovered
            .closed
            != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let snapshot = f.service.snapshot();
    assert!(snapshot.binding.active.is_none());
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.last_task.unwrap().phase, "abandoned");
    assert_eq!(
        f.service
            .core
            .writer()
            .unwrap()
            .load()
            .await
            .unwrap()
            .recovered,
        Counts {
            received: 1,
            submitting: 0,
            closed: 1
        }
    );
    assert!(f.bot.requests.lock().unwrap().is_empty());
    f.no_native_answer().await;

    // Registering the still-live synthetic source represents an explicit new
    // local binding; old authorization must not borrow its new generation.
    f.service.register(f.session.handle());
    f.bind().await;
    let replacement = f.service.snapshot().binding.active.unwrap();
    assert_ne!(replacement.generation, active.generation);
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        active.generation,
        &update(22, 22, "SYNTHETIC_OLD_RUN_INPUT"),
    )
    .await
    .unwrap();
    assert_eq!(f.service.snapshot().binding.active, Some(replacement));
    assert_eq!(f.service.snapshot().queued, 0);
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
    f.no_native_answer().await;
}

#[tokio::test]
async fn p2_invalidation_target_removal_blocks_late_prompt_ack_final_and_old_callback() {
    for method in ["thread/closed", "thread/unsubscribe"] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (bot, waiting, resume) = queued_cancel_delayed_bot().await;
        // Replace only the synthetic Bot endpoint, before any task owns it.
        f.service.core.inner.lock().unwrap().client = Some(bot.client.clone());
        f.bot = bot;
        let (job, mut attempt, cleanup) = f.owned_interaction_job().await;
        let active = job.active;
        f.native_interaction(&attempt, command_interaction(json!(73)))
            .await;
        let core = f.service.core.clone();
        let handle = attempt.interactions();
        let delivery = tokio::spawn(async move { interactions::offer(&core, &job, &handle).await });
        bounded(waiting).await.unwrap();
        let reference = button_reference(&f.prompt(0), "Allow once");
        assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
        let mut removal = json!({"method":method,"params":{"threadId":thread()}});
        if method == "thread/unsubscribe" {
            removal["id"] = json!(89);
            send(&mut f.tui, removal.clone()).await;
            assert_eq!(read(&mut f.native).await, removal);
        } else {
            send(&mut f.native, removal.clone()).await;
            assert_eq!(read(&mut f.tui).await, removal);
        }
        bounded(async {
            while f.session.handle().qualified(thread()) != Err(session::Failure::Unqualified) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if method == "thread/unsubscribe" {
            // Qualification closes on the outgoing request, before its ACK.
            // A late successful ACK cannot restore the former subscription.
            let response = json!({"id":89,"result":{}});
            send(&mut f.native, response.clone()).await;
            assert_eq!(read(&mut f.tui).await, response);
            assert_eq!(
                f.session.handle().qualified(thread()),
                Err(session::Failure::Unqualified)
            );
        }
        p2_invalidation_forward_late_final(&mut f).await;
        resume.send(()).unwrap();
        bounded(delivery).await.unwrap();
        // Keep Cleanup alive: absence must follow send's fresh qualification,
        // not the executor subsequently dropping and erasing a revived entry.
        assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
        assert!(bounded(attempt.completion()).await.is_err());
        process_update(
            &f.service.core,
            &f.bot.client,
            BOT,
            active.generation,
            &callback_update(21, "SYNTHETIC_REMOVED_TARGET_QUERY", &reference),
        )
        .await
        .unwrap();
        assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
        assert_eq!(callback_receipts(&f.journal()), 0);
        f.no_native_answer().await;
        drop(attempt);

        // A genuine new subscription round trip rotates target qualification;
        // delayed old input cannot revoke or answer through the replacement.
        send(
            &mut f.tui,
            json!({"id":90,"method":"thread/start","params":{}}),
        )
        .await;
        assert_eq!(read(&mut f.native).await["id"], 90);
        send(&mut f.native, json!({"id":90,"result":{"thread":{"id":thread(),"source":"cli","status":{"type":"idle"}}}})).await;
        read(&mut f.tui).await;
        bounded(async {
            while f.session.handle().qualified(thread()).is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        f.bind_with_consent().await;
        let replacement = f.service.snapshot().binding.active.unwrap();
        assert_ne!(replacement.generation, active.generation);
        assert_ne!(
            replacement.target.subscription_generation,
            active.target.subscription_generation
        );
        process_update(
            &f.service.core,
            &f.bot.client,
            BOT,
            active.generation,
            &callback_update(22, "SYNTHETIC_REBOUND_TARGET_QUERY", &reference),
        )
        .await
        .unwrap();
        assert_eq!(f.service.snapshot().binding.active, Some(replacement));
        assert_eq!(callback_receipts(&f.journal()), 0);
        assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
        drop(cleanup);
        assert_eq!(f.service.snapshot().binding.active, Some(replacement));
        f.no_native_answer().await;
    }
}

#[tokio::test]
async fn p2_invalidation_malformed_json_revokes_confirmed_authority_but_preserves_forwarding() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (task, active, executor) = p2_invalidation_start_interaction(&mut f).await;
    bounded(async {
        while f.service.core.inner.lock().unwrap().interactions.len() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let reference = button_reference(&f.prompt(0), "Allow once");
    f.admit(21, 21).await;
    assert_eq!(f.service.snapshot().queued, 1);
    const MALFORMED: &str = "{ \"synthetic_gap\": ";
    f.tui.send(Message::Text(MALFORMED.into())).await.unwrap();
    let forwarded = bounded(f.native.next()).await.unwrap().unwrap();
    assert_eq!(forwarded.to_text().unwrap(), MALFORMED);
    bounded(async {
        while f.session.snapshot().failure != Some("channel_protocol_invalid") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    p2_invalidation_forward_late_final(&mut f).await;
    assert!(bounded(executor).await.unwrap() == cancellations::ReceiptEnd::Close);
    process_update(
        &f.service.core,
        &f.bot.client,
        BOT,
        active.generation,
        &callback_update(22, "SYNTHETIC_GAP_QUERY", &reference),
    )
    .await
    .unwrap();
    let snapshot = f.service.snapshot();
    assert!(snapshot.binding.active.is_none());
    assert_eq!(snapshot.queued, 0);
    assert_eq!(snapshot.last_task.unwrap().id, task);
    assert_eq!(snapshot.last_task.unwrap().delivery, "withheld");
    assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
    assert_eq!(callback_receipts(&f.journal()), 0);
    assert_eq!(f.bot.requests.lock().unwrap().len(), 1);
    let records: Vec<Value> = f
        .journal()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["record"]["event"].clone())
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["kind"] == "submitting")
            .count(),
        1
    );
    assert!(!f.journal().contains("SYNTHETIC_GAP_QUERY"));
    f.no_native_answer().await;
}
