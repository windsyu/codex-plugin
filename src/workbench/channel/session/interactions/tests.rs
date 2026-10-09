use super::super::{Evidence, NativeStatus, Submission};
use super::*;
use serde_json::json;

fn attempt() -> PendingAttempt {
    super::super::tests::attempt()
}
fn turn() -> String {
    Uuid::from_u128(6).to_string()
}
fn command(a: &PendingAttempt, id: Value) -> Value {
    json!({"id":id,"method":"item/commandExecution/requestApproval","params":{
        "threadId":a.target.thread_id,"turnId":turn(),"itemId":"tool",
        "command":"printf synthetic", "availableDecisions":["accept","decline"]}})
}
fn observe(a: &mut PendingAttempt, direction: Direction, value: Value) {
    a.interactions
        .observe(direction, &value, a.target.thread_id);
}
fn own(a: &mut PendingAttempt, ack: bool, complete_user: bool) {
    let thread = a.target.thread_id.to_string();
    let client = a.client.to_string();
    let turn = turn();
    a.event(Evidence::TurnStarted {
        thread: &thread,
        turn: &turn,
    });
    a.event(Evidence::UserStarted {
        thread: &thread,
        turn: &turn,
        item: "user",
        client: Some(&client),
    });
    if complete_user {
        a.event(Evidence::UserCompleted {
            thread: &thread,
            turn: &turn,
            item: "user",
            client: Some(&client),
        });
    }
    if ack {
        a.acknowledge(Submission::Accepted {
            turn_id: Uuid::from_u128(6),
        });
    }
}
fn prompt(a: &mut PendingAttempt) -> Prompt {
    let value = command(a, json!(7));
    observe(a, Direction::ToTui, value);
    a.next_interaction().unwrap().unwrap()
}
#[test]
fn requests_before_ack_wait_for_complete_exact_user_proof_and_offer_once() {
    let mut a = attempt();
    let value = command(&a, json!(7));
    observe(&mut a, Direction::ToTui, value);
    assert!(a.next_interaction().unwrap().is_none());
    own(&mut a, false, true);
    assert!(a.next_interaction().unwrap().is_none());
    a.acknowledge(Submission::Accepted {
        turn_id: Uuid::from_u128(6),
    });
    let prompt = a.next_interaction().unwrap().unwrap();
    assert!(a.interaction_entry(prompt.id).is_ok());
    assert!(a.next_interaction().unwrap().is_none());
    let mut a = attempt();
    own(&mut a, true, false);
    let value = command(&a, json!(7));
    observe(&mut a, Direction::ToTui, value);
    assert!(a.next_interaction().unwrap().is_none());
}
#[test]
fn wrong_turn_is_never_exported_or_answerable() {
    let mut a = attempt();
    own(&mut a, true, true);
    let mut value = command(&a, json!(7));
    value["params"]["turnId"] = json!("other");
    observe(&mut a, Direction::ToTui, value);
    assert!(a.next_interaction().unwrap().is_none());
    let id = a.interactions.entries[0].id;
    assert!(a.interaction_entry(id).is_err());
}
#[test]
fn local_result_or_error_retires_only_exact_typed_rpc_id() {
    for response in [
        json!({"id":7,"result":{"decision":"accept"}}),
        json!({"id":7,"error":{"code":1}}),
    ] {
        let mut a = attempt();
        own(&mut a, true, true);
        let prompt = prompt(&mut a);
        observe(&mut a, Direction::ToServer, json!({"id":"7","result":{}}));
        assert!(a.interaction_entry(prompt.id).is_ok());
        observe(&mut a, Direction::ToServer, response);
        assert!(a.interaction_entry(prompt.id).is_err());
        assert!(a.failure.is_none());
        assert!(a.completion.is_none());
    }
}
#[test]
fn resolved_checks_thread_and_typed_request_id_without_claiming_completion() {
    let mut a = attempt();
    own(&mut a, true, true);
    let prompt = prompt(&mut a);
    for (thread, id) in [(Uuid::new_v4(), json!(7)), (a.target.thread_id, json!("7"))] {
        observe(
            &mut a,
            Direction::ToTui,
            json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":id}}),
        );
        assert!(a.interaction_entry(prompt.id).is_ok());
    }
    let thread = a.target.thread_id;
    observe(
        &mut a,
        Direction::ToTui,
        json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":7}}),
    );
    assert!(a.interaction_entry(prompt.id).is_err());
    assert!(a.completion.is_none());
}
#[test]
fn terminal_taken_failure_and_deadline_revoke_entries() {
    for mode in 0..5 {
        let mut a = attempt();
        own(&mut a, true, true);
        let prompt = prompt(&mut a);
        match mode {
            0 => a.event(Evidence::TurnCompleted {
                thread: &a.target.thread_id.to_string(),
                turn: &turn(),
                status: NativeStatus::Completed,
            }),
            1 => a.taken = true,
            2 => a.fail(Failure::InvalidEvidence),
            3 => a.interactions.entries[0].deadline = Instant::now() - Duration::from_secs(1),
            _ => a.event(Evidence::UserStarted {
                thread: &a.target.thread_id.to_string(),
                turn: &turn(),
                item: "local",
                client: None,
            }),
        }
        assert!(a.interaction_entry(prompt.id).is_err());
    }
}
#[test]
fn item_completion_retires_matching_item_only() {
    let mut a = attempt();
    own(&mut a, true, true);
    let prompt = prompt(&mut a);
    let thread = a.target.thread_id;
    for item in ["other", "tool"] {
        observe(
            &mut a,
            Direction::ToTui,
            json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn(),"item":{"id":item}}}),
        );
        assert_eq!(a.interaction_entry(prompt.id).is_ok(), item == "other");
    }
}
#[test]
fn duplicate_and_capacity_suppress_interactions_but_not_final_proof() {
    let mut a = attempt();
    own(&mut a, true, true);
    let prompt = prompt(&mut a);
    observe(&mut a, Direction::ToServer, json!({"id":7,"result":{}}));
    let duplicate = command(&a, json!(7));
    observe(&mut a, Direction::ToTui, duplicate);
    assert!(a.interaction_entry(prompt.id).is_err());
    assert!(a.interactions.failed && a.failure.is_none());
    assert_eq!(a.proof.live_owned_turn(), Some(turn().as_str()));
    let mut a = attempt();
    own(&mut a, true, true);
    for id in 0..=LIMIT {
        let value = command(&a, json!(id));
        observe(&mut a, Direction::ToTui, value);
        observe(&mut a, Direction::ToServer, json!({"id":id,"result":{}}));
    }
    assert!(a.interactions.failed && a.failure.is_none());
    assert_eq!(a.interactions.used.len(), LIMIT);
}
fn started(a: &PendingAttempt, path: &str) -> Value {
    json!({"method":"item/started","params":{"threadId":a.target.thread_id,"turnId":turn(),"item":{
        "id":"file","type":"fileChange","status":"inProgress","changes":[{"path":path,"kind":{"type":"add"},"diff":"PRIVATE_DIFF"}]}}})
}
fn file(a: &PendingAttempt) -> Value {
    json!({"id":"file","method":"item/fileChange/requestApproval","params":{"threadId":a.target.thread_id,"turnId":turn(),"itemId":"file"}})
}
#[test]
fn file_scope_requires_prior_full_started_and_conflicts_suppress() {
    for mode in 0..4 {
        let mut a = attempt();
        own(&mut a, true, true);
        let mut start = started(&a, "/synthetic/file.txt");
        match mode {
            0 => start["method"] = json!("item/fileChange/patchUpdated"),
            1 => start["params"]["threadId"] = json!(Uuid::new_v4()),
            2 => start["params"]["turnId"] = json!("other"),
            _ => {}
        }
        observe(&mut a, Direction::ToTui, start);
        let value = file(&a);
        observe(&mut a, Direction::ToTui, value);
        let p = a.next_interaction().unwrap();
        assert_eq!(p.is_some(), mode == 3);
        if let Some(p) = p {
            assert!(!p.request.text().contains("PRIVATE_DIFF"));
            let conflict = started(&a, "/synthetic/different.txt");
            observe(&mut a, Direction::ToTui, conflict);
            assert!(a.interaction_entry(p.id).is_err());
            assert!(a.failure.is_none());
        }
    }
}
#[test]
fn unsupported_own_scope_remains_local_without_poisoning_final_reply() {
    let mut a = attempt();
    own(&mut a, true, true);
    let mut value = command(&a, json!(7));
    value["params"]["additionalPermissions"] = json!({"network":true});
    observe(&mut a, Direction::ToTui, value);
    assert!(a.next_interaction().unwrap().is_none());
    assert!(a.interactions.notice_turns.contains(&turn()));
    assert!(a.failure.is_none());
    let mut foreign = command(&a, json!(8));
    foreign["params"]["threadId"] = json!(Uuid::new_v4());
    foreign["params"]["turnId"] = json!("foreign");
    observe(&mut a, Direction::ToTui, foreign);
    assert!(!a.interactions.notice_turns.contains("foreign"));
}

use crate::workbench::channel::native::{CaptureReceiver, Relay, RelayLimits};
use futures_util::{SinkExt, StreamExt};
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::Notify,
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async, client_async,
    tungstenite::{Message, protocol::Role},
};

struct Fixture {
    handle: InteractionHandle,
    prompt: Uuid,
    native: WebSocketStream<UnixStream>,
    tui: WebSocketStream<UnixStream>,
    upstream: WebSocketStream<UnixStream>,
    capture: CaptureReceiver,
    task: JoinHandle<()>,
    _relay: Relay,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = root.path().canonicalize().unwrap();
    let upstream_path = directory.join("up.sock");
    let listener = UnixListener::bind(&upstream_path).unwrap();
    let (relay, capture) = Relay::bind(
        directory.join("front.sock"),
        upstream_path,
        RelayLimits::default(),
    )
    .await
    .unwrap();
    let upstream = tokio::spawn(async move {
        accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap()
    });
    let tui = client_async(
        "ws://localhost/rpc",
        UnixStream::connect(relay.path()).await.unwrap(),
    )
    .await
    .unwrap()
    .0;
    let upstream = upstream.await.unwrap();
    let (client, server) = UnixStream::pair().unwrap();
    let native = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
    let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
    let (control, task) = crate::workbench::launch::ControlHandle::test_socket(socket);
    let mut a = attempt();
    a.target.transport_epoch = capture.state().connection_epoch.unwrap();
    own(&mut a, true, true);
    let prompt = prompt(&mut a).id;
    let target = a.target;
    let attempt_id = a.id;
    let mut state = State::new();
    state.epoch = Some(target.transport_epoch);
    state.sequence = capture.state().captured_sequence;
    state.targets = vec![super::super::Target {
        thread_id: target.thread_id,
        busy: true,
    }];
    state
        .generations
        .insert(target.thread_id, target.subscription_generation);
    state.attempt = Some(a);
    let handle = Handle {
        run_id: target.run_epoch,
        shared: Arc::new(super::super::Shared {
            state: Mutex::new(state),
            notify: Notify::new(),
        }),
        monitor: capture.subscribe_state(),
        control,
    };
    Fixture {
        handle: InteractionHandle::new(handle, attempt_id, target),
        prompt,
        native,
        tui,
        upstream,
        capture,
        task,
        _relay: relay,
        _root: root,
    }
}
#[tokio::test]
async fn actual_handle_checks_backlog_stale_attempt_and_generation() {
    let mut f = fixture().await;
    assert!(f.handle.current(f.prompt).is_ok());
    f.tui.send(Message::Text("{}".into())).await.unwrap();
    f.upstream.next().await.unwrap().unwrap();
    let frame = f.capture.recv().await.unwrap();
    assert_eq!(f.handle.current(f.prompt), Err(Failure::Backlog));
    {
        f.handle.handle.shared.state.lock().unwrap().sequence = frame.sequence;
    }
    assert!(f.handle.current(f.prompt).is_ok());
    let mut stale = f.handle.clone();
    stale.attempt = Uuid::new_v4();
    assert_eq!(stale.current(f.prompt), Err(Failure::StaleAttempt));
    let mut stale = f.handle.clone();
    stale.target.subscription_generation = Uuid::new_v4();
    assert_eq!(stale.current(f.prompt), Err(Failure::Unqualified));
    let mut stale = f.handle.clone();
    stale.target.run_epoch = Uuid::new_v4();
    assert_eq!(stale.current(f.prompt), Err(Failure::Unqualified));
    f.tui.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !f.handle.handle.monitor.state().closed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.handle.current(f.prompt), Err(Failure::Unavailable));
}
#[tokio::test]
async fn actor_guard_observes_external_revocation_and_fresh_session_gate() {
    for mode in 0..4 {
        let mut f = fixture().await;
        let shared = f.handle.handle.shared.clone();
        let prompt = f.prompt;
        let answer = f.handle.answer(
            prompt,
            Action::Accept,
            vec![],
            Box::new(move || {
                // Taking Session here proves external authorization ran before its lock.
                let mut state = shared.state.lock().unwrap();
                match mode {
                    0 => return false,
                    1 => {
                        state
                            .attempt
                            .as_mut()
                            .unwrap()
                            .interaction_entry(prompt)
                            .unwrap()
                            .retired = true
                    }
                    2 => state.sequence += 1,
                    _ => {
                        state.attempt.as_mut().unwrap().interactions.entries[0].deadline =
                            Instant::now() - Duration::from_secs(1)
                    }
                }
                true
            }),
        );
        let outcome = tokio::time::timeout(Duration::from_secs(2), answer)
            .await
            .unwrap();
        assert!(matches!(outcome, ResponseWrite::NotSent { .. }));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), f.native.next())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn actual_typed_response_is_consumed_before_socket_write_and_never_replayed() {
    let mut f = fixture().await;
    let response = f
        .handle
        .answer(f.prompt, Action::Decline, vec![], Box::new(|| true));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), response)
            .await
            .unwrap(),
        ResponseWrite::WrittenUnconfirmed
    );
    let message = tokio::time::timeout(Duration::from_secs(2), f.native.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap(),
        json!({"id":7,"result":{"decision":"decline"}})
    );
    assert!(f.handle.current(f.prompt).is_err());
    assert!(matches!(
        f.handle
            .answer(f.prompt, Action::Accept, vec![], Box::new(|| true))
            .await,
        ResponseWrite::NotSent { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.native.next())
            .await
            .is_err()
    );
}

#[test]
fn unsupported_question_tracks_only_identity_until_exact_native_retirement() {
    for mode in 0..4 {
        let mut a = attempt();
        own(&mut a, true, true);
        let thread = a.target.thread_id;
        observe(
            &mut a,
            Direction::ToTui,
            json!({"id":"question", "method":"item/tool/requestUserInput", "params":{
            "threadId":thread,"turnId":turn(),"itemId":"question-item", "questions":[{"isSecret":true,"question":"DO_NOT_EXPORT"}]}}),
        );
        assert!(a.next_interaction().unwrap().is_none());
        assert_eq!(a.awaiting_question(), Ok(true));
        match mode {
            0 => observe(
                &mut a,
                Direction::ToServer,
                json!({"id":"question","result":{}}),
            ),
            1 => observe(
                &mut a,
                Direction::ToTui,
                json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":"question"}}),
            ),
            2 => observe(
                &mut a,
                Direction::ToTui,
                json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn(),"item":{"id":"question-item"}}}),
            ),
            _ => observe(
                &mut a,
                Direction::ToServer,
                json!({"id":"question","error":{"code":1}}),
            ),
        }
        assert_eq!(a.awaiting_question(), Ok(false));
        assert!(a.failure.is_none());
    }
}

#[test]
fn expired_remote_question_still_blocks_text_until_native_retirement() {
    let mut a = attempt();
    own(&mut a, true, true);
    let thread = a.target.thread_id;
    observe(
        &mut a,
        Direction::ToTui,
        json!({"id":"question", "method":"item/tool/requestUserInput", "params":{
        "threadId":thread,"turnId":turn(),"itemId":"question-item", "questions":[{
            "id":"choice", "header":"Choice", "question":"Continue?", "isOther":false, "isSecret":false,
            "options":[{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]
        }]}}),
    );
    let prompt = a.next_interaction().unwrap().unwrap();
    assert!(a.interaction_entry(prompt.id).is_ok());
    a.interactions.entries[0].deadline = Instant::now() - Duration::from_secs(1);
    assert!(a.interaction_entry(prompt.id).is_err());
    assert!(a.next_interaction().unwrap().is_none());
    assert_eq!(a.awaiting_question(), Ok(true));
    observe(
        &mut a,
        Direction::ToTui,
        json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":"other"}}),
    );
    assert_eq!(a.awaiting_question(), Ok(true));
    observe(
        &mut a,
        Direction::ToTui,
        json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":"question"}}),
    );
    assert_eq!(a.awaiting_question(), Ok(false));
}

#[tokio::test]
async fn written_question_answer_blocks_text_until_native_resolved() {
    let mut f = fixture().await;
    let thread = f.handle.target.thread_id;
    {
        let mut state = f.handle.handle.shared.state.lock().unwrap();
        let a = state.attempt.as_mut().unwrap();
        observe(
            a,
            Direction::ToTui,
            json!({"id":"question", "method":"item/tool/requestUserInput", "params":{
            "threadId":thread,"turnId":turn(),"itemId":"question-item", "questions":[{
                "id":"choice", "header":"Choice", "question":"Continue?", "isOther":false, "isSecret":false,
                "options":[{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]
            }]}}),
        );
    }
    let prompt = f.handle.next().unwrap().unwrap();
    assert_eq!(f.handle.awaiting_question(), Ok(true));
    assert_eq!(
        f.handle
            .answer(
                prompt.id,
                Action::Choose {
                    question: 0,
                    option: 0
                },
                vec![],
                Box::new(|| true)
            )
            .await,
        ResponseWrite::WrittenUnconfirmed
    );
    let message = tokio::time::timeout(Duration::from_secs(2), f.native.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap()["id"],
        json!("question")
    );
    assert!(f.handle.current(prompt.id).is_err());
    assert_eq!(f.handle.awaiting_question(), Ok(true));
    {
        let mut state = f.handle.handle.shared.state.lock().unwrap();
        observe(
            state.attempt.as_mut().unwrap(),
            Direction::ToTui,
            json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":"question"}}),
        );
    }
    assert_eq!(f.handle.awaiting_question(), Ok(false));
}

#[test]
fn question_classification_requires_ack_and_complete_owned_user_proof() {
    for question in [false, true] {
        for ack in [false, true] {
            let mut a = attempt();
            assert_eq!(a.awaiting_question(), Err(Failure::InteractionUnavailable));
            own(&mut a, ack, !ack);
            let thread = a.target.thread_id;
            if question {
                observe(
                    &mut a,
                    Direction::ToTui,
                    json!({"id":"question", "method":"item/tool/requestUserInput", "params":{
                    "threadId":thread,"turnId":turn(),"itemId":"question-item", "questions":[{"isSecret":true,"question":"DO_NOT_EXPORT"}]}}),
                );
            }
            // With or without an observed question, the missing ACK or user
            // completion must keep text admission closed and payload withheld.
            assert_eq!(a.awaiting_question(), Err(Failure::InteractionUnavailable));
            assert!(a.next_interaction().unwrap().is_none());
            if ack {
                a.event(Evidence::UserCompleted {
                    thread: &thread.to_string(),
                    turn: &turn(),
                    item: "user",
                    client: Some(&a.client.to_string()),
                });
            } else {
                a.acknowledge(Submission::Accepted {
                    turn_id: Uuid::from_u128(6),
                });
            }
            assert_eq!(a.awaiting_question(), Ok(question));
        }
    }
}

fn mixed_question_value(thread: Uuid) -> Value {
    json!({"id":"mixed-question", "method":"item/tool/requestUserInput", "params":{
    "threadId":thread,"turnId":turn(),"itemId":"mixed-item", "questions":[{
        "id":"choice", "header":"Choice", "question":"Continue?", "isOther":false, "isSecret":false,
        "options":[{"label":"Yes","description":"Continue"},{"label":"No","description":"Stop"}]
    }, {
        "id":"details", "header":"Details", "question":"Describe the task", "isOther":false, "isSecret":false,
        "options":null
    }]}})
}
fn install_question(f: &Fixture, value: Value) -> Option<Prompt> {
    {
        let mut state = f.handle.handle.shared.state.lock().unwrap();
        observe(state.attempt.as_mut().unwrap(), Direction::ToTui, value);
    }
    f.handle.next().unwrap()
}
async fn assert_no_native_write(f: &mut Fixture) {
    assert!(
        tokio::time::timeout(Duration::from_millis(30), f.native.next())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn mixed_text_and_choice_emit_one_complete_typed_answer_in_either_order() {
    for text_last in [false, true] {
        let mut f = fixture().await;
        let prompt = install_question(&f, mixed_question_value(f.handle.target.thread_id)).unwrap();
        // Neither a lone text answer nor a lone choice may reach the socket.
        assert!(matches!(
            f.handle
                .answer_text(
                    prompt.id,
                    1,
                    "Synthetic details".into(),
                    vec![],
                    Box::new(|| true)
                )
                .await,
            ResponseWrite::NotSent { .. }
        ));
        assert!(matches!(
            f.handle
                .answer(
                    prompt.id,
                    Action::Choose {
                        question: 0,
                        option: 0
                    },
                    vec![],
                    Box::new(|| true)
                )
                .await,
            ResponseWrite::NotSent { .. }
        ));
        assert!(f.handle.current(prompt.id).is_ok());
        assert_no_native_write(&mut f).await;
        let result = if text_last {
            let selected = vec![prompt.request.select(0, 0).unwrap()];
            f.handle
                .answer_text(
                    prompt.id,
                    1,
                    "Synthetic details".into(),
                    selected,
                    Box::new(|| true),
                )
                .await
        } else {
            let selected = vec![prompt.request.answer_text(1, "Synthetic details").unwrap()];
            f.handle
                .answer(
                    prompt.id,
                    Action::Choose {
                        question: 0,
                        option: 0,
                    },
                    selected,
                    Box::new(|| true),
                )
                .await
        };
        assert_eq!(result, ResponseWrite::WrittenUnconfirmed);
        let message = tokio::time::timeout(Duration::from_secs(2), f.native.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap(),
            json!({"id":"mixed-question","result":{"answers":{"choice":{"answers":["Yes"]},"details":{"answers":["Synthetic details"]}}}})
        );
        assert!(f.handle.current(prompt.id).is_err());
        assert_eq!(f.handle.awaiting_question(), Ok(true));
        assert!(matches!(
            f.handle
                .answer_text(
                    prompt.id,
                    1,
                    "Replay".into(),
                    vec![prompt.request.select(0, 0).unwrap()],
                    Box::new(|| true)
                )
                .await,
            ResponseWrite::NotSent { .. }
        ));
        assert_no_native_write(&mut f).await;
    }
}
#[tokio::test]
async fn text_answers_reject_choice_only_bounds_invalid_text_and_duplicate_answers() {
    let mut f = fixture().await;
    let prompt = install_question(&f, mixed_question_value(f.handle.target.thread_id)).unwrap();
    for (question, text, duplicate) in [
        (0, "Synthetic", false),
        (2, "Synthetic", false),
        (1, "", false),
        (1, "  ", false),
        (1, "bad\u{0}", false),
        (1, "Synthetic", true),
    ] {
        let selected = if duplicate {
            vec![prompt.request.answer_text(1, "Earlier").unwrap()]
        } else {
            vec![prompt.request.select(0, 0).unwrap()]
        };
        let result = f
            .handle
            .answer_text(
                prompt.id,
                question,
                text.into(),
                selected,
                Box::new(|| true),
            )
            .await;
        assert!(matches!(result, ResponseWrite::NotSent { .. }));
        assert!(f.handle.current(prompt.id).is_ok());
    }
    assert_no_native_write(&mut f).await;
}
#[tokio::test]
async fn secret_or_unsupported_question_never_acquires_text_answer_authority() {
    for secret in [false, true] {
        let mut f = fixture().await;
        let mut value = mixed_question_value(f.handle.target.thread_id);
        if secret {
            value["params"]["questions"][1]["isSecret"] = json!(true);
        } else {
            value["params"]["questions"][1]["unknownField"] = json!("unsupported");
        }
        assert!(install_question(&f, value).is_none());
        assert_eq!(f.handle.awaiting_question(), Ok(true));
        assert!(matches!(
            f.handle
                .answer_text(Uuid::new_v4(), 1, "Text".into(), vec![], Box::new(|| true))
                .await,
            ResponseWrite::NotSent { .. }
        ));
        assert_no_native_write(&mut f).await;
    }
}
#[tokio::test]
async fn text_answer_actor_revalidates_resolution_consent_source_and_expiry() {
    for mode in 0..5 {
        let mut f = fixture().await;
        let prompt = install_question(&f, mixed_question_value(f.handle.target.thread_id)).unwrap();
        let selected = vec![prompt.request.select(0, 0).unwrap()];
        let shared = f.handle.handle.shared.clone();
        let thread = f.handle.target.thread_id;
        let id = prompt.id;
        let result = tokio::time::timeout(Duration::from_secs(2), f.handle.answer_text(id, 1, "Text".into(), selected, Box::new(move || {
            let mut state = shared.state.lock().unwrap();
            match mode {
                0 => return false,
                1 => observe(state.attempt.as_mut().unwrap(), Direction::ToTui, json!({"method":"serverRequest/resolved","params":{"threadId":thread,"requestId":"mixed-question"}})),
                2 => observe(state.attempt.as_mut().unwrap(), Direction::ToServer, json!({"id":"mixed-question","result":{}})),
                3 => state.sequence += 1,
                _ => state.attempt.as_mut().unwrap().interaction_entry(id).unwrap().deadline = Instant::now() - Duration::from_secs(1),
            }
            true
        }))).await.unwrap();
        assert!(matches!(result, ResponseWrite::NotSent { .. }));
        assert_no_native_write(&mut f).await;
    }
}
