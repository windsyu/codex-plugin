use super::*;
use serde_json::json;
fn target() -> QualifiedTarget {
    QualifiedTarget::new(
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        Uuid::from_u128(3),
        Uuid::from_u128(4),
    )
    .unwrap()
}
pub(super) fn attempt() -> PendingAttempt {
    let target = target();
    let client = Uuid::from_u128(5);
    PendingAttempt {
        id: Uuid::new_v4(),
        target,
        client,
        proof: OriginProof::new(&target.thread_id.to_string(), client).unwrap(),
        submitted: true,
        acknowledged: false,
        acknowledged_turn: None,
        terminal: None,
        completion: None,
        taken: false,
        failure: None,
        interactions: interactions::Pending::default(),
    }
}
fn finish(a: &mut PendingAttempt) {
    let thread = a.target.thread_id.to_string();
    let client = a.client.to_string();
    let turn = Uuid::from_u128(6).to_string();
    for event in [
        Evidence::TurnStarted {
            thread: &thread,
            turn: &turn,
        },
        Evidence::UserStarted {
            thread: &thread,
            turn: &turn,
            item: "user",
            client: Some(&client),
        },
        Evidence::UserCompleted {
            thread: &thread,
            turn: &turn,
            item: "user",
            client: Some(&client),
        },
        Evidence::FinalAnswer {
            thread: &thread,
            turn: &turn,
            item: "final",
            text: "synthetic private body",
        },
        Evidence::TurnCompleted {
            thread: &thread,
            turn: &turn,
            status: NativeStatus::Completed,
        },
    ] {
        a.event(event)
    }
}
#[test]
fn late_ack_is_required_and_mismatch_cannot_release_body() {
    let mut a = attempt();
    finish(&mut a);
    assert!(a.take_completion().unwrap().is_none());
    a.acknowledge(Submission::Accepted {
        turn_id: Uuid::from_u128(6),
    });
    assert_eq!(
        a.take_completion().unwrap().unwrap().reply.as_deref(),
        Some("synthetic private body")
    );
    assert!(matches!(a.take_completion(), Err(Failure::CompletionTaken)));
    let mut a = attempt();
    finish(&mut a);
    a.acknowledge(Submission::Accepted {
        turn_id: Uuid::from_u128(7),
    });
    assert!(matches!(a.take_completion(), Err(Failure::InvalidEvidence)));
}
#[test]
fn late_user_or_local_input_revokes_completed_proof() {
    let mut a = attempt();
    finish(&mut a);
    let thread = a.target.thread_id.to_string();
    a.event(Evidence::UserStarted {
        thread: &thread,
        turn: "late",
        item: "late",
        client: None,
    });
    a.acknowledge(Submission::Accepted {
        turn_id: Uuid::from_u128(6),
    });
    assert!(matches!(a.take_completion(), Err(Failure::InvalidEvidence)));
    assert!(evidence::local_input(
        &json!({"method":"turn/steer","params":{"threadId":thread}}),
        a.target.thread_id
    ));
    assert!(!evidence::local_input(
        &json!({"method":"turn/steer","params":{"threadId":Uuid::from_u128(9)}}),
        a.target.thread_id
    ));
}
#[test]
fn unknown_submission_never_returns_saved_completion() {
    let mut a = attempt();
    finish(&mut a);
    a.acknowledge(Submission::Unknown {
        reason: ControlFailure::Timeout,
    });
    assert!(matches!(a.take_completion(), Err(Failure::Submission)));
}
#[test]
fn backlog_gap_and_epoch_are_checked_against_atomic_source() {
    let target = target();
    let mut state = State::new();
    state.publish(
        target.transport_epoch,
        8,
        vec![Target {
            thread_id: target.thread_id,
            busy: false,
        }],
    );
    let mut capture = CaptureState {
        captured_sequence: 9,
        connection_epoch: Some(target.transport_epoch),
        gap_generation: 0,
        closed: false,
        reason: None,
    };
    assert_eq!(state.check_source(capture, false), Err(Failure::Backlog));
    capture.captured_sequence = 8;
    assert_eq!(state.check_source(capture, false), Ok(()));
    capture.gap_generation = 1;
    assert_eq!(
        state.check_source(capture, false),
        Err(Failure::Unavailable)
    );
    assert!(state.targets.is_empty());
}
#[test]
fn unsubscribe_and_requalification_rotate_generation_and_poison_old_attempt() {
    let target = target();
    let mut state = State::new();
    state.epoch = Some(target.transport_epoch);
    state.targets = vec![Target {
        thread_id: target.thread_id,
        busy: false,
    }];
    state
        .generations
        .insert(target.thread_id, target.subscription_generation);
    state.attempt = Some(attempt());
    state.publish(target.transport_epoch, 1, vec![]);
    state.publish(
        target.transport_epoch,
        2,
        vec![Target {
            thread_id: target.thread_id,
            busy: false,
        }],
    );
    assert_eq!(
        state.check_target(target.run_epoch, target),
        Err(Failure::Unqualified)
    );
    assert_eq!(
        state.attempt.as_ref().unwrap().failure,
        Some(Failure::Unqualified)
    );
}
#[test]
fn evidence_requires_target_shapes_and_known_items() {
    let thread = target().thread_id;
    for item in [
        json!({"type":"futureItem","id":"x"}),
        json!({"type":"agentMessage","id":"x","text":"private","phase":"future"}),
        json!({"type":"userMessage","id":"x","clientId":3,"content":[]}),
    ] {
        assert!(evidence::parse(&json!({"method":"item/completed","params":{"threadId":thread,"turnId":"t","item":item}}),thread).is_err());
    }
    assert!(
        evidence::parse(
            &json!({"method":"unrelated/future","params":{"body":"private"}}),
            thread
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn serializable_snapshot_has_no_attempt_or_body() {
    let snapshot = Snapshot {
        run_id: Uuid::new_v4(),
        revision: Uuid::new_v4(),
        transport_epoch: None,
        phase: "waiting",
        targets: vec![],
        failure: None,
    };
    let value = serde_json::to_value(snapshot).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 6);
    assert!(value.get("attempt").is_none());
}

#[test]
fn failed_terminal_without_own_user_settles_only_after_ack() {
    let mut a = attempt();
    let thread = a.target.thread_id.to_string();
    let turn = Uuid::from_u128(6).to_string();
    a.event(Evidence::TurnStarted {
        thread: &thread,
        turn: &turn,
    });
    a.event(Evidence::TurnCompleted {
        thread: &thread,
        turn: &turn,
        status: NativeStatus::Failed,
    });
    assert!(a.take_completion().unwrap().is_none());
    a.acknowledge(Submission::Accepted {
        turn_id: Uuid::from_u128(6),
    });
    let result = a.take_completion().unwrap().unwrap();
    assert_eq!(result.status, NativeStatus::Failed);
    assert!(result.reply.is_none());
    assert!(result.withheld.is_some());
}
#[test]
fn double_arm_and_stale_drop_do_not_remove_new_attempt() {
    let target = target();
    let mut state = State::new();
    state.epoch = Some(target.transport_epoch);
    state.targets = vec![Target {
        thread_id: target.thread_id,
        busy: false,
    }];
    state
        .generations
        .insert(target.thread_id, target.subscription_generation);
    let first = state
        .arm(target.run_epoch, target, Uuid::from_u128(5))
        .unwrap();
    assert_eq!(
        state.arm(target.run_epoch, target, Uuid::from_u128(6)),
        Err(Failure::AttemptActive)
    );
    state.remove_attempt(first);
    let second = state
        .arm(target.run_epoch, target, Uuid::from_u128(6))
        .unwrap();
    state.remove_attempt(first);
    assert_eq!(state.attempt.unwrap().id, second);
}

#[test]
fn submission_is_consumed_before_rpc_and_cannot_be_retried() {
    let mut a = attempt();
    a.submitted = false;
    assert!(a.begin_submit(true));
    assert!(!a.begin_submit(true));
    assert_eq!(a.failure, Some(Failure::Submission));
    let mut a = attempt();
    a.submitted = false;
    assert!(!a.begin_submit(false));
    assert!(!a.begin_submit(true));
}
#[test]
fn close_or_epoch_change_invalidates_before_decoder_catches_up() {
    for changed_epoch in [false, true] {
        let target = target();
        let mut state = State::new();
        state.publish(
            target.transport_epoch,
            1,
            vec![Target {
                thread_id: target.thread_id,
                busy: false,
            }],
        );
        state.attempt = Some(attempt());
        let capture = CaptureState {
            captured_sequence: 1,
            connection_epoch: Some(if changed_epoch {
                Uuid::new_v4()
            } else {
                target.transport_epoch
            }),
            gap_generation: 0,
            closed: !changed_epoch,
            reason: None,
        };
        assert_eq!(
            state.check_source(capture, false),
            Err(Failure::Unavailable)
        );
        assert_eq!(state.attempt.unwrap().failure, Some(Failure::Unavailable));
    }
}
