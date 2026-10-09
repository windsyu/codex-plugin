use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}
fn key(message_id: i64) -> DeliveryKey {
    DeliveryKey {
        bot_id: 101,
        chat_id: 202,
        message_id,
    }
}
async fn shutdown(mut worker: Worker) {
    let thread = worker.thread.take().unwrap();
    drop(worker);
    // Tests explicitly await teardown off the async executor; production Drop
    // never joins. Keeping Handle clones is intentional in the callers below.
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
}
async fn pause(handle: &Handle) -> mpsc::Sender<()> {
    let (entered, waiting) = oneshot::channel();
    let (resume, receiver) = mpsc::channel();
    handle
        .send(Operation::Pause {
            entered,
            resume: receiver,
        })
        .unwrap();
    waiting.await.unwrap();
    resume
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_metadata_is_durable_across_worker_restart_and_never_recovered_as_work() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    assert!(!handle.is_rejected(101, 7, Some(key(7))).await.unwrap());
    handle.reject(101, 7, Some(key(7))).await.unwrap();
    shutdown(worker).await;
    let worker = Worker::start(path).unwrap();
    let handle = worker.handle();
    assert!(handle.is_rejected(101, 7, None).await.unwrap());
    assert!(handle.is_rejected(101, 99, Some(key(7))).await.unwrap());
    assert_eq!(
        handle.admit(key(7), 99, Uuid::from_u128(1)).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(handle.load().await.unwrap().recovered, Counts::default());
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn counts_and_credentials_recover_without_replaying_and_writer_is_exclusive() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let initial = handle.load().await.unwrap();
    assert!(initial.token.is_none());
    assert_eq!(initial.recovered, Counts::default());
    for id in 1..=3 {
        assert_eq!(
            handle.admit(key(id), id, Uuid::from_u128(id as u128)).await,
            Ok(Admission::Fresh)
        );
    }
    handle
        .mark_submitting(Uuid::from_u128(2), Uuid::from_u128(22))
        .await
        .unwrap();
    handle.close(Uuid::from_u128(3)).await.unwrap();
    handle
        .save_token("101:synthetic_only".to_owned())
        .await
        .unwrap();
    let expected = Counts {
        received: 1,
        submitting: 1,
        closed: 1,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    let second = Worker::start(path.clone()).unwrap();
    assert!(matches!(
        second.handle().load().await,
        Err(Failure::Store(store::Failure::Busy))
    ));
    shutdown(worker).await;
    assert!(matches!(handle.load().await, Err(Failure::Stopped)));
    // A failed open stays failed. Releasing the first writer cannot silently
    // promote an already-failed worker into an authorized replacement writer.
    assert!(matches!(
        second.handle().load().await,
        Err(Failure::Store(store::Failure::Busy))
    ));
    shutdown(second).await;
    let reopened = Worker::start(path).unwrap();
    let handle = reopened.handle();
    let loaded = handle.load().await.unwrap();
    assert_eq!(loaded.recovered, expected);
    assert_eq!(loaded.token.as_deref(), Some("101:synthetic_only"));
    assert_eq!(
        handle.admit(key(2), 99, Uuid::from_u128(999)).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle
            .mark_submitting(Uuid::from_u128(2), Uuid::from_u128(222))
            .await,
        Err(Failure::Store(store::Failure::InvalidTransition))
    );
    handle.remove_token().await.unwrap();
    assert!(handle.load().await.unwrap().token.is_none());
    shutdown(reopened).await;
}

#[tokio::test(flavor = "current_thread")]
async fn stalled_storage_keeps_runtime_live_queue_bounded_and_drop_nonblocking() {
    let root = root();
    let mut worker = Worker::start(root.path().join("receipts")).unwrap();
    let handle = worker.handle();
    handle.load().await.unwrap();
    // Hold the storage thread exactly as a blocked filesystem operation would.
    // No production timing hook or Store implementation change is needed.
    let resume = pause(&handle).await;
    let mut replies = Vec::new();
    for _ in 0..QUEUE_LIMIT {
        let (reply, result) = oneshot::channel();
        handle.send(Operation::Load(reply)).unwrap();
        replies.push(result);
    }
    assert!(matches!(handle.load().await, Err(Failure::QueueFull)));
    let heartbeat = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        7
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), heartbeat)
            .await
            .unwrap()
            .unwrap(),
        7
    );
    // Qualification invalidation has no dependency on the stalled store thread.
    let now = std::time::Instant::now();
    let mut binding = crate::workbench::channel::binding::Binding::new();
    let before = binding.snapshot(now).generation;
    binding.revoke();
    assert_ne!(binding.snapshot(now).generation, before);
    let thread = worker.thread.take().unwrap();
    let dropped = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || drop(worker)),
    )
    .await;
    // Release the test gate even on a regression so runtime shutdown cannot hang.
    resume.send(()).unwrap();
    assert!(
        matches!(dropped, Ok(Ok(()))),
        "owner drop must not join stalled storage"
    );
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    for reply in replies {
        assert!(reply.await.is_err());
    }
    assert!(matches!(handle.load().await, Err(Failure::Stopped)));
    assert_eq!(Failure::QueueFull.code(), "channel_receipts_queue_full");
}

#[tokio::test(flavor = "current_thread")]
async fn lost_caller_after_durable_intent_retains_uncertainty_and_never_resubmits() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(11);
    let client = Uuid::from_u128(12);
    handle.admit(key(1), 1, task).await.unwrap();
    let (entered, waiting) = oneshot::channel();
    let (resume, pause) = mpsc::channel();
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::PauseAfterIntent {
            task,
            client,
            entered,
            resume: pause,
            reply,
        })
        .unwrap();
    waiting.await.unwrap(); // real Store::mark_submitting and fsync completed
    drop(result); // response is lost after durable intent, like a canceled actor
    resume.send(()).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 1,
        closed: 0,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
    let worker = Worker::start(path).unwrap();
    let handle = worker.handle();
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(
        handle.mark_submitting(task, client).await,
        Err(Failure::Store(store::Failure::InvalidTransition))
    );
    assert_eq!(
        handle.admit(key(1), 2, Uuid::from_u128(13)).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_queued_work_is_not_written_and_bad_open_is_async_static_failure() {
    let root = root();
    let worker = Worker::start(root.path().join("receipts")).unwrap();
    let handle = worker.handle();
    handle.load().await.unwrap();
    let resume = pause(&handle).await;
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::Admit {
            key: key(1),
            update_id: 1,
            task: Uuid::from_u128(1),
            reply,
        })
        .unwrap();
    drop(result);
    resume.send(()).unwrap();
    assert_eq!(handle.load().await.unwrap().recovered, Counts::default());
    let invalid = "credential must not appear in an error".to_owned();
    let failure = handle.save_token(invalid.clone()).await.unwrap_err();
    assert_eq!(failure.code(), "channel_receipts_invalid_input");
    assert!(!format!("{failure:?} {failure}").contains(&invalid));
    shutdown(worker).await;
    // Missing parent is observed only on the storage thread. Worker::start is
    // safe for an async caller even when opening the root will fail.
    let worker = Worker::start(root.path().join("missing").join("receipts")).unwrap();
    let error = match worker.handle().load().await {
        Err(error) => error,
        Ok(_) => panic!("missing private parent must reject open"),
    };
    assert_eq!(error.code(), "channel_receipts_unavailable");
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_callback_result_retains_only_dedup_metadata_across_worker_restart() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    handle.admit(key(1), 1, task).await.unwrap();
    handle
        .mark_submitting(task, Uuid::from_u128(3))
        .await
        .unwrap();
    let (entered, waiting) = oneshot::channel();
    let (resume, pause) = mpsc::channel();
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::PauseAfterCallback {
            task,
            interaction,
            entered,
            resume: pause,
            reply,
        })
        .unwrap();
    waiting.await.unwrap(); // actual callback append and fsync completed
    drop(result);
    resume.send(()).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 1,
        closed: 0,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(
        handle
            .admit_callback(101, 21, "synthetic-query".into(), task, interaction)
            .await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle
            .admit_callback(101, 20, "different-query".into(), task, interaction)
            .await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_queued_callback_is_not_written_and_query_bytes_are_bounded_before_queueing() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    handle.admit(key(1), 1, task).await.unwrap();
    handle
        .mark_submitting(task, Uuid::from_u128(3))
        .await
        .unwrap();
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let resume = pause(&handle).await;
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::AdmitCallback {
            bot_id: 101,
            update_id: 20,
            query_id: "cancelled-query".into(),
            task,
            interaction,
            reply,
        })
        .unwrap();
    drop(result);
    for query in ["".to_owned(), "q\n".into(), "q".repeat(1025)] {
        // Completes while the worker is paused: validation precedes queueing.
        assert_eq!(
            handle
                .admit_callback(101, 20, query, task, interaction)
                .await,
            Err(Failure::Store(store::Failure::InvalidInput))
        );
    }
    resume.send(()).unwrap();
    handle.load().await.unwrap();
    assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    assert_eq!(
        handle
            .admit_callback(101, 20, "cancelled-query".into(), task, interaction)
            .await,
        Ok(Admission::Fresh)
    );
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_queued_question_reply_has_no_receipt_and_does_not_become_task() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    handle.admit(key(1), 1, task).await.unwrap();
    handle
        .mark_submitting(task, Uuid::from_u128(3))
        .await
        .unwrap();
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let resume = pause(&handle).await;
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::AdmitQuestionReply {
            key: key(20),
            update_id: 20,
            task,
            interaction,
            reply,
        })
        .unwrap();
    drop(result);
    resume.send(()).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 1,
        closed: 0,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    assert_eq!(
        handle
            .admit_question_reply(key(20), 20, task, interaction)
            .await,
        Ok(Admission::Fresh)
    );
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_question_reply_result_survives_restart_as_dedup_metadata_only() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    handle.admit(key(1), 1, task).await.unwrap();
    handle
        .mark_submitting(task, Uuid::from_u128(3))
        .await
        .unwrap();
    let (entered, waiting) = oneshot::channel();
    let (resume, paused) = mpsc::channel();
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::PauseAfterQuestionReply {
            task,
            interaction,
            entered,
            resume: paused,
            reply,
        })
        .unwrap();
    waiting.await.unwrap(); // real receipt write and fsync, only IDs went to worker
    drop(result);
    resume.send(()).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 1,
        closed: 0,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(
        handle
            .admit_question_reply(key(20), 21, task, interaction)
            .await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle
            .admit_question_reply(key(21), 20, task, interaction)
            .await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle.admit(key(20), 22, Uuid::from_u128(4)).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle
            .admit_callback(
                101,
                20,
                "query-cannot-reuse-text-update".into(),
                task,
                interaction
            )
            .await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_cancel_lost_result_is_durable_closed_with_no_replay_on_worker_restart() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    handle.admit(key(1), 1, task).await.unwrap();
    let (entered, waiting) = oneshot::channel();
    let (resume, paused) = mpsc::channel();
    let (reply, result) = oneshot::channel();
    handle
        .send(Operation::PauseAfterCancel {
            task,
            entered,
            resume: paused,
            reply,
        })
        .unwrap();
    waiting.await.unwrap(); // actual cancel append and fsync already completed
    drop(result);
    resume.send(()).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 0,
        closed: 1,
    };
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
    let worker = Worker::start(path).unwrap();
    let handle = worker.handle();
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    assert_eq!(
        handle.cancel_queued(key(20), 21, task).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle.cancel_queued(key(21), 20, task).await,
        Ok(Admission::Duplicate)
    );
    assert_eq!(
        handle.mark_submitting(task, Uuid::from_u128(2)).await,
        Err(Failure::Store(store::Failure::InvalidTransition))
    );
    assert_eq!(
        handle.close(task).await,
        Err(Failure::Store(store::Failure::InvalidTransition))
    );
    assert_eq!(handle.load().await.unwrap().recovered, expected);
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queued_cancel_waits_off_executor_is_bounded_and_dropped_queue_entry_does_not_close() {
    let root = root();
    let worker = Worker::start(root.path().join("receipts")).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    handle.admit(key(1), 1, task).await.unwrap();
    let resume = pause(&handle).await;
    let mut replies = Vec::new();
    for id in 0..QUEUE_LIMIT - 1 {
        let (reply, result) = oneshot::channel();
        handle
            .send(Operation::CancelQueued {
                key: key(id as i64 + 10),
                update_id: id as i64 + 10,
                task,
                reply,
            })
            .unwrap();
        replies.push(result);
    }
    let (barrier, drained) = oneshot::channel();
    handle.send(Operation::Load(barrier)).unwrap();
    assert_eq!(
        handle.cancel_queued(key(50), 50, task).await,
        Err(Failure::QueueFull)
    );
    let heartbeat = tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        7
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), heartbeat)
            .await
            .unwrap()
            .unwrap(),
        7
    );
    drop(replies);
    resume.send(()).unwrap();
    assert_eq!(
        drained.await.unwrap().unwrap().recovered,
        Counts {
            received: 1,
            submitting: 0,
            closed: 0
        }
    );
    assert_eq!(
        handle.cancel_queued(key(10), 10, task).await,
        Ok(Admission::Fresh)
    );
    assert_eq!(
        handle.load().await.unwrap().recovered,
        Counts {
            received: 0,
            submitting: 0,
            closed: 1
        }
    );
    shutdown(worker).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_input_query_roundtrips_consumed_and_fresh_identities_without_new_tasks() {
    let root = root();
    let worker = Worker::start(root.path().join("receipts")).unwrap();
    let handle = worker.handle();
    let task = Uuid::from_u128(1);
    handle.admit(key(1), 1, task).await.unwrap();
    assert_eq!(handle.cancel_input_seen(key(1), 2).await, Ok(true));
    assert_eq!(handle.cancel_input_seen(key(2), 1).await, Ok(true));
    assert_eq!(handle.cancel_input_seen(key(2), 2).await, Ok(false));
    assert_eq!(
        handle.cancel_input_seen(key(0), 2).await,
        Err(Failure::Store(store::Failure::InvalidInput))
    );
    handle.cancel_queued(key(2), 2, task).await.unwrap();
    assert_eq!(handle.cancel_input_seen(key(3), 2).await, Ok(true));
    assert_eq!(handle.cancel_input_seen(key(2), 3).await, Ok(true));
    assert_eq!(handle.cancel_input_seen(key(3), 3).await, Ok(false));
    assert_eq!(
        handle.load().await.unwrap().recovered,
        Counts {
            received: 0,
            submitting: 0,
            closed: 1
        }
    );
    shutdown(worker).await;
    assert_eq!(
        handle.cancel_input_seen(key(3), 3).await,
        Err(Failure::Stopped)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn input_seen_worker_roundtrips_all_domains_without_append_or_identity_learning() {
    let root = root();
    let path = root.path().join("receipts");
    let mut worker = Worker::start(path.clone()).unwrap();
    assert_ne!(
        worker.thread.as_ref().unwrap().thread().id(),
        std::thread::current().id()
    );
    let mut handle = worker.handle();
    let active = Uuid::from_u128(1);
    let interaction = Uuid::from_u128(2);
    let canceled = Uuid::from_u128(9);
    handle.admit(key(1), 1, active).await.unwrap();
    handle
        .mark_submitting(active, Uuid::from_u128(3))
        .await
        .unwrap();
    handle.admit(key(9), 9, canceled).await.unwrap();
    handle.reject(101, 20, Some(key(20))).await.unwrap();
    handle
        .admit_callback(
            101,
            30,
            "SYNTHETIC_INPUT_SEEN_QUERY".to_owned(),
            active,
            interaction,
        )
        .await
        .unwrap();
    handle
        .admit_question_reply(key(40), 40, active, interaction)
        .await
        .unwrap();
    handle.cancel_queued(key(50), 50, canceled).await.unwrap();
    let digest = *blake3::hash(b"SYNTHETIC_INPUT_SEEN_QUERY").as_bytes();
    let unknown_digest = *blake3::hash(b"SYNTHETIC_UNSEEN_QUERY").as_bytes();
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let expected = Counts {
        received: 0,
        submitting: 1,
        closed: 1,
    };
    for reopen in [false, true] {
        if reopen {
            shutdown(worker).await;
            worker = Worker::start(path.clone()).unwrap();
            handle = worker.handle();
        }
        for update in [1, 9, 20, 30, 40, 50] {
            assert_eq!(handle.input_seen(101, update, None, None).await, Ok(true));
            assert_eq!(handle.input_seen(102, update, None, None).await, Ok(false));
        }
        for message in [1, 9, 20, 40, 50] {
            assert_eq!(
                handle.input_seen(101, 100, Some(key(message)), None).await,
                Ok(true)
            );
            assert_eq!(
                handle
                    .input_seen(
                        102,
                        100,
                        Some(DeliveryKey {
                            bot_id: 102,
                            ..key(message)
                        }),
                        None
                    )
                    .await,
                Ok(false)
            );
        }
        assert_eq!(
            handle.input_seen(101, 100, None, Some(digest)).await,
            Ok(true)
        );
        assert_eq!(
            handle.input_seen(102, 100, None, Some(digest)).await,
            Ok(false)
        );
        assert_eq!(
            handle
                .input_seen(101, 1, Some(key(100)), Some(unknown_digest))
                .await,
            Ok(true)
        );
        assert_eq!(
            handle
                .input_seen(101, 100, Some(key(1)), Some(unknown_digest))
                .await,
            Ok(true)
        );
        assert_eq!(
            handle
                .input_seen(101, 100, Some(key(100)), Some(digest))
                .await,
            Ok(true)
        );
        assert_eq!(
            handle
                .input_seen(101, 100, Some(key(100)), Some(unknown_digest))
                .await,
            Ok(false)
        );
        for (bot, update, input) in [
            (0, 1, None),
            (101, -1, None),
            (102, 1, Some(key(1))),
            (101, 1, Some(key(0))),
            (
                101,
                1,
                Some(DeliveryKey {
                    chat_id: 0,
                    ..key(1)
                }),
            ),
        ] {
            assert_eq!(
                handle.input_seen(bot, update, input, Some(digest)).await,
                Err(Failure::Store(store::Failure::InvalidInput))
            );
        }
        assert_eq!(handle.load().await.unwrap().recovered, expected);
        assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    }
    shutdown(worker).await;
    assert_eq!(
        handle.input_seen(101, 100, None, None).await,
        Err(Failure::Stopped)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn input_seen_queued_lookup_waits_off_executor_and_dropped_caller_cannot_append() {
    let root = root();
    let path = root.path().join("receipts");
    let worker = Worker::start(path.clone()).unwrap();
    assert_ne!(
        worker.thread.as_ref().unwrap().thread().id(),
        std::thread::current().id()
    );
    let handle = worker.handle();
    handle.admit(key(1), 1, Uuid::from_u128(1)).await.unwrap();
    let before = fs::read(path.join("receipts.jsonl")).unwrap();
    let resume = pause(&handle).await;
    let digest = *blake3::hash(b"SYNTHETIC_DROPPED_INPUT_LOOKUP").as_bytes();
    let mut lookup = Box::pin(handle.input_seen(101, 1, Some(key(100)), Some(digest)));
    // Poll the actual public Handle future once, so the OS-thread request is
    // queued before dropping its response receiver. A blocked worker cannot
    // answer; the sole Tokio executor nevertheless continues to run.
    let pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(std::future::Future::poll(lookup.as_mut(), cx).is_pending())
    })
    .await;
    drop(lookup);
    let heartbeat = tokio::spawn(async {
        tokio::task::yield_now().await;
        7
    });
    let heartbeat = tokio::time::timeout(Duration::from_secs(1), heartbeat).await;
    // Release before assertions, so a failed assertion never strands the worker.
    resume.send(()).unwrap();
    assert!(pending, "input lookup must pass through the paused worker");
    assert!(matches!(heartbeat, Ok(Ok(7))));
    // This load is queued after the dropped request and serves as a drain
    // barrier. Neither the dropped query nor later reads may learn its aliases.
    assert_eq!(
        handle.load().await.unwrap().recovered,
        Counts {
            received: 1,
            submitting: 0,
            closed: 0
        }
    );
    assert_eq!(
        handle
            .input_seen(101, 100, Some(key(100)), Some(digest))
            .await,
        Ok(false)
    );
    assert_eq!(handle.input_seen(101, 1, None, None).await, Ok(true));
    assert_eq!(fs::read(path.join("receipts.jsonl")).unwrap(), before);
    shutdown(worker).await;
}
