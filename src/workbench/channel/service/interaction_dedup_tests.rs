// A conflicting duplicate must not consume a different, still-current prompt.
// These exercise the production update classifier, durable worker and native
// control socket, then prove that a fresh answer can still complete normally.

#[tokio::test]
async fn interaction_duplicate_identity_received_update_preserves_current_command() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, command_interaction(json!("dedup-command-rpc")))
        .await;
    f.offer_interaction(&job, &attempt).await;
    let reference = button_reference(&f.prompt(0), "Allow once");
    let pending: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(pending.len(), 1);
    let before = f.journal();
    let requests_before = f.bot.requests.lock().unwrap().len();
    let query = "SYNTHETIC_DUPLICATE_COMMAND_QUERY";
    // owned_interaction_job durably received update 20. Its identity must not
    // acquire this current button, nor learn the otherwise-fresh query ID.
    f.process_callback(&callback_update(20, query, &reference))
        .await;
    assert_eq!(f.journal(), before);
    let requests_after = f.bot.requests.lock().unwrap().len();
    assert_eq!(requests_after, requests_before);
    f.no_native_answer().await;
    let current: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(
        current, pending,
        "a duplicate update consumed the current command prompt"
    );
    f.process_callback(&callback_update(21, query, &reference))
        .await;
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"dedup-command-rpc","result":{"decision":"accept"}})
    );
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert_eq!(question_reply_receipts(&f.journal()), 0);
    let empty = f.service.core.inner.lock().unwrap().interactions.is_empty();
    assert!(empty);
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(
        bounded(f.service.core.writer().unwrap().load())
            .await
            .unwrap()
            .recovered,
        Counts {
            received: 0,
            submitting: 1,
            closed: 0
        }
    );
    for private in [
        BODY,
        "SYNTHETIC_APPROVAL_BODY",
        "dedup-command-rpc",
        query,
        &reference,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_duplicate_identity_received_message_preserves_current_text_question() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(
        &attempt,
        one_question(json!("dedup-text-rpc"), Value::Null, false),
    )
    .await;
    f.offer_interaction(&job, &attempt).await;
    assert_eq!(f.prompt(0)["reply_markup"]["force_reply"], true);
    let pending: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(pending.len(), 1);
    let before = f.journal();
    let requests_before = f.bot.requests.lock().unwrap().len();
    let text = "SYNTHETIC_DUPLICATE_TEXT_ANSWER 😀";
    // The original task's message 20 is already durable, even though update 21
    // is fresh and the Reply metadata names the current question correctly.
    f.process_callback(&question_reply_update(21, 20, 77, text))
        .await;
    assert_eq!(f.journal(), before);
    let requests_after = f.bot.requests.lock().unwrap().len();
    assert_eq!(requests_after, requests_before);
    f.no_native_answer().await;
    let current: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(
        current, pending,
        "a duplicate message consumed the current text question"
    );
    f.process_callback(&question_reply_update(22, 21, 77, text))
        .await;
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"dedup-text-rpc","result":{"answers":{
            "original-question":{"answers":[text]}
        }}})
    );
    f.no_native_answer().await;
    assert_eq!(question_reply_receipts(&f.journal()), 1);
    assert_eq!(callback_receipts(&f.journal()), 0);
    let empty = f.service.core.inner.lock().unwrap().interactions.is_empty();
    assert!(empty);
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(
        bounded(f.service.core.writer().unwrap().load())
            .await
            .unwrap()
            .recovered,
        Counts {
            received: 0,
            submitting: 1,
            closed: 0
        }
    );
    for private in [
        BODY,
        "SYNTHETIC_FREE_QUESTION",
        "SYNTHETIC_QUOTED_BODY_MUST_NOT_BE_RETAINED",
        "dedup-text-rpc",
        text,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_duplicate_identity_prior_query_preserves_next_question_and_fresh_update() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, json!({"id":"dedup-two-questions-rpc","method":"item/tool/requestUserInput","params":{
        "threadId":thread(),"turnId":interaction_turn(),"itemId":"question-item","isBlocking":true,
        "questions":[
            {"id":"first-original","header":"First","question":"SYNTHETIC_DEDUP_QUESTION_ONE","isOther":false,"isSecret":false,
             "options":[{"label":"First A","description":"A"},{"label":"First B","description":"B"}]},
            {"id":"second-original","header":"Second","question":"SYNTHETIC_DEDUP_QUESTION_TWO","isOther":false,"isSecret":false,
             "options":[{"label":"Second A","description":"A"},{"label":"Second B","description":"B"}]}
        ]}})).await;
    f.offer_interaction(&job, &attempt).await;
    let first = button_reference(&f.prompt(0), "1.2");
    let query_a = "SYNTHETIC_DEDUP_QUERY_A";
    let query_b = "SYNTHETIC_DEDUP_QUERY_B";
    f.process_callback(&callback_update(21, query_a, &first))
        .await;
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 1);
    let second = button_reference(&f.prompt(1), "2.1");
    assert_ne!(first, second);
    let pending: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(pending.len(), 1);
    let before = f.journal();
    let requests_before = f.bot.requests.lock().unwrap().len();
    // Use the new question's real prompt ID and reference, but the first
    // question's consumed query identity. Update 22 must remain unconsumed.
    let mut duplicate = callback_update(22, query_a, &second);
    duplicate["callback_query"]["message"]["message_id"] = json!(78);
    f.process_callback(&duplicate).await;
    assert_eq!(f.journal(), before);
    let requests_after = f.bot.requests.lock().unwrap().len();
    assert_eq!(requests_after, requests_before);
    f.no_native_answer().await;
    let current: Vec<_> = f
        .service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect();
    assert_eq!(
        current, pending,
        "a consumed query identity removed the next question"
    );
    let mut fresh = callback_update(22, query_b, &second);
    fresh["callback_query"]["message"]["message_id"] = json!(78);
    f.process_callback(&fresh).await;
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"dedup-two-questions-rpc","result":{"answers":{
            "first-original":{"answers":["First B"]},
            "second-original":{"answers":["Second A"]}
        }}})
    );
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 2);
    assert_eq!(question_reply_receipts(&f.journal()), 0);
    let empty = f.service.core.inner.lock().unwrap().interactions.is_empty();
    assert!(empty);
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(
        bounded(f.service.core.writer().unwrap().load())
            .await
            .unwrap()
            .recovered,
        Counts {
            received: 0,
            submitting: 1,
            closed: 0
        }
    );
    for private in [
        BODY,
        "SYNTHETIC_DEDUP_QUESTION_ONE",
        "SYNTHETIC_DEDUP_QUESTION_TWO",
        "dedup-two-questions-rpc",
        "First B",
        "Second A",
        query_a,
        query_b,
        &first,
        &second,
    ] {
        assert!(!f.journal().contains(private));
    }
}

fn interaction_dedup_pending(f: &Fixture) -> Vec<Uuid> {
    f.service
        .core
        .inner
        .lock()
        .unwrap()
        .interactions
        .keys()
        .copied()
        .collect()
}

#[tokio::test]
async fn interaction_duplicate_identity_paused_callbacks_preserve_other_live_reference() {
    for collision in ["update", "query"] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        for (id, item) in [
            ("paused-command-a", "command-a"),
            ("paused-command-b", "command-b"),
        ] {
            let mut request = command_interaction(json!(id));
            request["params"]["itemId"] = json!(item);
            f.native_interaction(&attempt, request).await;
            f.offer_interaction(&job, &attempt).await;
        }
        let first = button_reference(&f.prompt(0), "Allow once");
        let second = button_reference(&f.prompt(1), "Deny");
        let pending = interaction_dedup_pending(&f);
        assert_eq!(pending.len(), 2);
        let before = f.journal();
        let core = f.service.core.clone();
        let client = f.bot.client.clone();
        let generation = f.service.snapshot().binding.generation;
        let writer = core.writer().unwrap();
        let resume = bounded(writer.pause_for_test()).await.unwrap();
        let first_update = callback_update(21, "SYNTHETIC_PAUSED_QUERY_A", &first);
        let mut receiving = Box::pin(process_update(
            &core,
            &client,
            BOT,
            generation,
            &first_update,
        ));
        assert!(futures_util::poll!(receiving.as_mut()).is_pending());
        let mut duplicate = if collision == "update" {
            callback_update(21, "SYNTHETIC_PAUSED_QUERY_B", &second)
        } else {
            callback_update(22, "SYNTHETIC_PAUSED_QUERY_A", &second)
        };
        duplicate["callback_query"]["message"]["message_id"] = json!(78);
        let mut repeated = Box::pin(process_update(&core, &client, BOT, generation, &duplicate));
        // The other prompt must be protected before even the read-only durable
        // query can finish. Waiting for the paused worker here would miss it.
        assert!(matches!(
            futures_util::poll!(repeated.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        drop(repeated);
        assert_eq!(interaction_dedup_pending(&f), pending);
        assert_eq!(f.journal(), before);
        f.no_native_answer().await;
        resume.send(()).unwrap();
        bounded(receiving).await.unwrap();
        assert_eq!(
            read(&mut f.control).await,
            json!({"id":"paused-command-a","result":{"decision":"accept"}})
        );
        assert_eq!(callback_receipts(&f.journal()), 1);
        let pending = interaction_dedup_pending(&f);
        assert_eq!(pending.len(), 1);
        let before = f.journal();
        // Now the same query is durably consumed. This duplicate must also
        // preserve B, without learning update 22 or the earlier query B.
        let mut durable_duplicate = callback_update(22, "SYNTHETIC_PAUSED_QUERY_A", &second);
        durable_duplicate["callback_query"]["message"]["message_id"] = json!(78);
        f.process_callback(&durable_duplicate).await;
        assert_eq!(interaction_dedup_pending(&f), pending);
        assert_eq!(f.journal(), before);
        f.no_native_answer().await;
        let mut fresh = callback_update(22, "SYNTHETIC_PAUSED_QUERY_B", &second);
        fresh["callback_query"]["message"]["message_id"] = json!(78);
        f.process_callback(&fresh).await;
        assert_eq!(
            read(&mut f.control).await,
            json!({"id":"paused-command-b","result":{"decision":"decline"}})
        );
        f.no_native_answer().await;
        assert!(interaction_dedup_pending(&f).is_empty());
        assert_eq!(callback_receipts(&f.journal()), 2);
        assert_eq!(f.service.snapshot().queued, 0);
        for private in [
            BODY,
            "SYNTHETIC_APPROVAL_BODY",
            "SYNTHETIC_PAUSED_QUERY_A",
            "SYNTHETIC_PAUSED_QUERY_B",
            "paused-command-a",
            "paused-command-b",
            &first,
            &second,
        ] {
            assert!(!f.journal().contains(private));
        }
    }
}

#[tokio::test]
async fn interaction_duplicate_identity_paused_text_shares_update_and_delivery_domains() {
    for callback_first in [true, false] {
        let mut f = Fixture::new(false).await;
        f.bind_with_consent().await;
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        let first_request = if callback_first {
            command_interaction(json!("shared-first-rpc"))
        } else {
            one_question(json!("shared-first-rpc"), Value::Null, false)
        };
        f.native_interaction(&attempt, first_request).await;
        f.offer_interaction(&job, &attempt).await;
        let mut second_request = one_question(json!("shared-second-rpc"), Value::Null, false);
        second_request["params"]["itemId"] = json!("second-question-item");
        f.native_interaction(&attempt, second_request).await;
        f.offer_interaction(&job, &attempt).await;
        let pending = interaction_dedup_pending(&f);
        assert_eq!(pending.len(), 2);
        let first_update = if callback_first {
            callback_update(
                21,
                "SYNTHETIC_SHARED_QUERY",
                &button_reference(&f.prompt(0), "Allow once"),
            )
        } else {
            question_reply_update(21, 21, 77, "SYNTHETIC_SHARED_FIRST_TEXT")
        };
        let duplicate = if callback_first {
            question_reply_update(21, 22, 78, "SYNTHETIC_SHARED_SECOND_TEXT")
        } else {
            question_reply_update(22, 21, 78, "SYNTHETIC_SHARED_SECOND_TEXT")
        };
        let before = f.journal();
        let core = f.service.core.clone();
        let client = f.bot.client.clone();
        let generation = f.service.snapshot().binding.generation;
        let writer = core.writer().unwrap();
        let resume = bounded(writer.pause_for_test()).await.unwrap();
        let mut receiving = Box::pin(process_update(
            &core,
            &client,
            BOT,
            generation,
            &first_update,
        ));
        assert!(futures_util::poll!(receiving.as_mut()).is_pending());
        let mut repeated = Box::pin(process_update(&core, &client, BOT, generation, &duplicate));
        assert!(matches!(
            futures_util::poll!(repeated.as_mut()),
            std::task::Poll::Ready(Ok(()))
        ));
        drop(repeated);
        assert_eq!(interaction_dedup_pending(&f), pending);
        assert_eq!(f.journal(), before);
        f.no_native_answer().await;
        resume.send(()).unwrap();
        bounded(receiving).await.unwrap();
        let expected = if callback_first {
            json!({"id":"shared-first-rpc","result":{"decision":"accept"}})
        } else {
            json!({"id":"shared-first-rpc","result":{"answers":{"original-question":{"answers":["SYNTHETIC_SHARED_FIRST_TEXT"]}}}})
        };
        assert_eq!(read(&mut f.control).await, expected);
        let pending = interaction_dedup_pending(&f);
        assert_eq!(pending.len(), 1);
        let before = f.journal();
        f.process_callback(&duplicate).await;
        assert_eq!(interaction_dedup_pending(&f), pending);
        assert_eq!(f.journal(), before);
        f.no_native_answer().await;
        // Reuse both otherwise-new identities from the conflicting duplicates.
        f.process_callback(&question_reply_update(
            22,
            22,
            78,
            "SYNTHETIC_SHARED_SECOND_TEXT",
        ))
        .await;
        assert_eq!(
            read(&mut f.control).await,
            json!({"id":"shared-second-rpc","result":{"answers":{"original-question":{"answers":["SYNTHETIC_SHARED_SECOND_TEXT"]}}}})
        );
        f.no_native_answer().await;
        assert!(interaction_dedup_pending(&f).is_empty());
        assert_eq!(callback_receipts(&f.journal()), usize::from(callback_first));
        assert_eq!(
            question_reply_receipts(&f.journal()),
            if callback_first { 1 } else { 2 }
        );
        assert_eq!(f.service.snapshot().queued, 0);
        for private in [
            BODY,
            "SYNTHETIC_SHARED_QUERY",
            "SYNTHETIC_SHARED_FIRST_TEXT",
            "SYNTHETIC_SHARED_SECOND_TEXT",
            "SYNTHETIC_FREE_QUESTION",
            "shared-first-rpc",
            "shared-second-rpc",
        ] {
            assert!(!f.journal().contains(private));
        }
    }
}

#[tokio::test]
async fn interaction_duplicate_identity_lost_caller_keeps_reservation_without_consuming_prompt() {
    let mut f = Fixture::new(false).await;
    f.bind_with_consent().await;
    let (job, attempt, _cleanup) = f.owned_interaction_job().await;
    f.native_interaction(&attempt, command_interaction(json!("lost-input-rpc")))
        .await;
    f.offer_interaction(&job, &attempt).await;
    let reference = button_reference(&f.prompt(0), "Allow once");
    let pending = interaction_dedup_pending(&f);
    let before = f.journal();
    let core = f.service.core.clone();
    let client = f.bot.client.clone();
    let generation = f.service.snapshot().binding.generation;
    let writer = core.writer().unwrap();
    let resume = bounded(writer.pause_for_test()).await.unwrap();
    let update = callback_update(21, "SYNTHETIC_LOST_QUERY", &reference);
    let mut receiving = Box::pin(process_update(&core, &client, BOT, generation, &update));
    assert!(futures_util::poll!(receiving.as_mut()).is_pending());
    drop(receiving);
    let mut repeated = Box::pin(process_update(&core, &client, BOT, generation, &update));
    assert!(matches!(
        futures_util::poll!(repeated.as_mut()),
        std::task::Poll::Ready(Ok(()))
    ));
    drop(repeated);
    assert_eq!(interaction_dedup_pending(&f), pending);
    assert_eq!(f.journal(), before);
    resume.send(()).unwrap();
    // A worker barrier proves the lost query has finished/skipped; an empty
    // durable lookup must not make the abandoned volatile reservation reusable.
    bounded(writer.load()).await.unwrap();
    f.process_callback(&update).await;
    assert_eq!(interaction_dedup_pending(&f), pending);
    assert_eq!(f.journal(), before);
    f.no_native_answer().await;
    f.process_callback(&callback_update(
        22,
        "SYNTHETIC_FRESH_AFTER_LOST",
        &reference,
    ))
    .await;
    assert_eq!(
        read(&mut f.control).await,
        json!({"id":"lost-input-rpc","result":{"decision":"accept"}})
    );
    f.no_native_answer().await;
    assert_eq!(callback_receipts(&f.journal()), 1);
    assert!(interaction_dedup_pending(&f).is_empty());
    for private in [
        BODY,
        "SYNTHETIC_LOST_QUERY",
        "SYNTHETIC_FRESH_AFTER_LOST",
        "lost-input-rpc",
        &reference,
    ] {
        assert!(!f.journal().contains(private));
    }
}

#[tokio::test]
async fn interaction_duplicate_identity_abandoned_input_reservations_are_bounded() {
    let mut f = Fixture::new(false).await;
    let core = f.service.core.clone();
    let client = f.bot.client.clone();
    let generation = f.service.snapshot().binding.generation;
    let writer = core.writer().unwrap();
    let before = f.journal();
    // Drain each canceled read separately so this tests the retained identity
    // budget (17), independently of the receipt worker's queue capacity (32).
    for id in 100..117 {
        let resume = bounded(writer.pause_for_test()).await.unwrap();
        let value = update(id, id, "SYNTHETIC_ABANDONED_INPUT");
        let mut receiving = Box::pin(process_update(&core, &client, BOT, generation, &value));
        assert!(futures_util::poll!(receiving.as_mut()).is_pending());
        drop(receiving);
        resume.send(()).unwrap();
        bounded(writer.load()).await.unwrap();
    }
    assert_eq!(f.journal(), before);
    assert_eq!(f.service.snapshot().queued, 0);
    f.no_native_answer().await;
    // Existing reserved identities still suppress duplicates at capacity.
    let old = update(100, 100, "SYNTHETIC_ABANDONED_INPUT");
    let mut repeated = Box::pin(process_update(&core, &client, BOT, generation, &old));
    assert!(matches!(
        futures_util::poll!(repeated.as_mut()),
        std::task::Poll::Ready(Ok(()))
    ));
    drop(repeated);
    let fresh = update(117, 117, "SYNTHETIC_OVER_CAPACITY_INPUT");
    let mut receiving = Box::pin(process_update(&core, &client, BOT, generation, &fresh));
    assert!(matches!(
        futures_util::poll!(receiving.as_mut()),
        std::task::Poll::Ready(Err(()))
    ));
    drop(receiving);
    assert_eq!(f.service.snapshot().failure, Some("channel_input_capacity"));
    assert_eq!(f.service.snapshot().queued, 0);
    assert_eq!(f.journal(), before);
    f.no_native_answer().await;
}
