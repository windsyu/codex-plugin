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
