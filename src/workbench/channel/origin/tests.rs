use super::*;

const THREAD: &str = "synthetic-thread";
const TURN: &str = "synthetic-turn";

fn start(proof: &mut OriginProof, client: &str) {
    proof.observe(Evidence::TurnStarted {
        thread: THREAD,
        turn: TURN,
    });
    proof.observe(Evidence::UserStarted {
        thread: THREAD,
        turn: TURN,
        item: "user",
        client: Some(client),
    });
    proof.observe(Evidence::UserCompleted {
        thread: THREAD,
        turn: TURN,
        item: "user",
        client: Some(client),
    });
}
fn answer(proof: &mut OriginProof) {
    assert!(
        proof
            .observe(Evidence::FinalAnswer {
                thread: THREAD,
                turn: TURN,
                item: "answer",
                text: "synthetic final"
            })
            .is_none()
    );
}
fn finish(proof: &mut OriginProof, status: NativeStatus) -> Completion {
    proof
        .observe(Evidence::TurnCompleted {
            thread: THREAD,
            turn: TURN,
            status,
        })
        .unwrap()
}

#[test]
fn native_completion_waits_for_full_input_lifecycle_and_delivers_once() {
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    answer(&mut proof);
    // Submission RPC response can arrive after the notifications, or never arrive.
    proof.observe(Evidence::SubmissionTurn(TURN));
    let done = finish(&mut proof, NativeStatus::Completed);
    assert!(done.withheld.is_none());
    assert_eq!(done.reply.as_deref(), Some("synthetic final"));
    assert!(
        proof
            .observe(Evidence::TurnCompleted {
                thread: THREAD,
                turn: TURN,
                status: NativeStatus::Completed
            })
            .is_none()
    );
}

#[test]
fn local_steer_even_after_final_answer_suppresses_the_reply() {
    for after_final in [false, true] {
        let client = Uuid::new_v4();
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        start(&mut proof, &client.to_string());
        if after_final {
            answer(&mut proof);
        }
        proof.observe(Evidence::UserStarted {
            thread: THREAD,
            turn: TURN,
            item: "local",
            client: None,
        });
        proof.observe(Evidence::UserCompleted {
            thread: THREAD,
            turn: TURN,
            item: "local",
            client: None,
        });
        if !after_final {
            answer(&mut proof);
        }
        let done = finish(&mut proof, NativeStatus::Completed);
        assert_eq!(done.withheld, Some(Withheld::MixedInput));
        assert!(done.reply.is_none());
    }
}

#[test]
fn local_turn_winning_submission_race_and_reused_client_are_mixed() {
    for local_client in [None, Some(Uuid::nil().to_string())] {
        let client = Uuid::new_v4();
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        proof.observe(Evidence::TurnStarted {
            thread: THREAD,
            turn: TURN,
        });
        proof.observe(Evidence::UserStarted {
            thread: THREAD,
            turn: TURN,
            item: "local",
            client: local_client.as_deref(),
        });
        proof.observe(Evidence::UserCompleted {
            thread: THREAD,
            turn: TURN,
            item: "local",
            client: local_client.as_deref(),
        });
        proof.observe(Evidence::UserStarted {
            thread: THREAD,
            turn: TURN,
            item: "user",
            client: Some(&client.to_string()),
        });
        answer(&mut proof);
        assert!(finish(&mut proof, NativeStatus::Completed).reply.is_none());
    }
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    proof.observe(Evidence::UserStarted {
        thread: THREAD,
        turn: TURN,
        item: "second",
        client: Some(&client.to_string()),
    });
    assert_eq!(
        finish(&mut proof, NativeStatus::Completed).withheld,
        Some(Withheld::MixedInput)
    );
}

#[test]
fn duplicates_are_tolerated_but_missing_started_event_or_client_conflict_are_not() {
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    start(&mut proof, &client.to_string());
    answer(&mut proof);
    assert!(finish(&mut proof, NativeStatus::Completed).reply.is_some());
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    proof.observe(Evidence::TurnStarted {
        thread: THREAD,
        turn: TURN,
    });
    proof.observe(Evidence::UserCompleted {
        thread: THREAD,
        turn: TURN,
        item: "user",
        client: Some(&client.to_string()),
    });
    assert_eq!(
        finish(&mut proof, NativeStatus::Completed).withheld,
        Some(Withheld::IncompleteEvidence)
    );
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    proof.observe(Evidence::UserCompleted {
        thread: THREAD,
        turn: TURN,
        item: "user",
        client: None,
    });
    assert_eq!(
        finish(&mut proof, NativeStatus::Completed).withheld,
        Some(Withheld::MixedInput)
    );
}

#[test]
fn gaps_unknown_status_and_disagreeing_rpc_identity_do_not_export() {
    for status in [
        NativeStatus::Unknown,
        NativeStatus::Failed,
        NativeStatus::Interrupted,
    ] {
        let client = Uuid::new_v4();
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        start(&mut proof, &client.to_string());
        answer(&mut proof);
        assert_eq!(
            finish(&mut proof, status).withheld,
            Some(Withheld::UnsuccessfulTurn)
        );
    }
    for event in [Evidence::Gap, Evidence::SubmissionTurn("different-turn")] {
        let client = Uuid::new_v4();
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        start(&mut proof, &client.to_string());
        proof.observe(event);
        answer(&mut proof);
        assert!(finish(&mut proof, NativeStatus::Completed).reply.is_none());
    }
}

#[test]
fn bounded_reply_and_late_user_events_fail_closed() {
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    proof.observe(Evidence::FinalAnswer {
        thread: THREAD,
        turn: TURN,
        item: "answer",
        text: &"x".repeat(MAX_REPLY + 1),
    });
    assert_eq!(
        finish(&mut proof, NativeStatus::Completed).withheld,
        Some(Withheld::ReplyLimit)
    );
    proof.observe(Evidence::UserStarted {
        thread: THREAD,
        turn: TURN,
        item: "late",
        client: None,
    });
    assert_eq!(proof.invalidated(), Some(Withheld::InvalidOrder));
}

#[test]
fn answer_preceding_owned_input_cannot_be_adopted_by_later_input() {
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    proof.observe(Evidence::TurnStarted {
        thread: THREAD,
        turn: TURN,
    });
    answer(&mut proof);
    start(&mut proof, &client.to_string());
    let done = finish(&mut proof, NativeStatus::Completed);
    assert_eq!(done.withheld, Some(Withheld::IncompleteEvidence));
    assert!(done.reply.is_none());
}

#[test]
fn prior_local_evidence_and_terminal_identity_cannot_be_erased() {
    let client = Uuid::new_v4();
    for completed in [false, true] {
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        if completed {
            proof.observe(Evidence::TurnStarted {
                thread: THREAD,
                turn: TURN,
            });
        }
        proof.observe(Evidence::UserStarted {
            thread: THREAD,
            turn: TURN,
            item: "local",
            client: None,
        });
        if completed {
            proof.observe(Evidence::TurnCompleted {
                thread: THREAD,
                turn: TURN,
                status: NativeStatus::Completed,
            });
        }
        start(&mut proof, &client.to_string());
        answer(&mut proof);
        let done = proof.observe(Evidence::TurnCompleted {
            thread: THREAD,
            turn: TURN,
            status: NativeStatus::Completed,
        });
        assert!(done.is_none_or(|done| done.withheld.is_some() && done.reply.is_none()));
        assert!(proof.invalidated().is_some());
    }
}

#[test]
fn unexplained_interleaving_and_terminal_overflow_disable_export() {
    let client = Uuid::new_v4();
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    start(&mut proof, &client.to_string());
    proof.observe(Evidence::FinalAnswer {
        thread: THREAD,
        turn: "other-turn",
        item: "other-answer",
        text: "synthetic local",
    });
    answer(&mut proof);
    assert!(finish(&mut proof, NativeStatus::Completed).reply.is_none());
    let mut proof = OriginProof::new(THREAD, client).unwrap();
    for index in 0..=MAX_TERMINALS {
        proof.observe(Evidence::TurnCompleted {
            thread: THREAD,
            turn: &format!("local-{index}"),
            status: NativeStatus::Completed,
        });
    }
    start(&mut proof, &client.to_string());
    answer(&mut proof);
    assert_eq!(
        finish(&mut proof, NativeStatus::Completed).withheld,
        Some(Withheld::CaptureGap)
    );
}

#[test]
fn final_item_replays_do_not_replace_newer_answer_or_hide_identity_conflict() {
    let client = Uuid::new_v4();
    for changed in [false, true] {
        let mut proof = OriginProof::new(THREAD, client).unwrap();
        start(&mut proof, &client.to_string());
        for (item, text) in [
            ("a", "first"),
            ("b", "second"),
            ("a", if changed { "changed" } else { "first" }),
        ] {
            proof.observe(Evidence::FinalAnswer {
                thread: THREAD,
                turn: TURN,
                item,
                text,
            });
        }
        let done = finish(&mut proof, NativeStatus::Completed);
        if changed {
            assert_eq!(done.withheld, Some(Withheld::IdentityConflict));
            assert!(done.reply.is_none());
        } else {
            assert_eq!(done.reply.as_deref(), Some("second"));
        }
    }
}
