use super::*;
use serde_json::json;

fn target() -> QualifiedTarget {
    QualifiedTarget::new(
        Uuid::from_u128(11),
        Uuid::from_u128(12),
        Uuid::from_u128(13),
        Uuid::from_u128(14),
    )
    .unwrap()
}
fn message(text: &str) -> Value {
    json!({"update_id": 1, "message": {
        "message_id": 2, "date": 3,
        "from": {"id": 101, "is_bot": false},
        "chat": {"id": 202, "type": "private"}, "text": text
    }})
}
fn pair(binding: &mut Binding, now: Instant) -> (PairingCode, Candidate) {
    let code = binding.begin_pair(BotIdentity { id: 99 }, now).unwrap();
    let mut update = message(&format!("/bind {}", code.nonce));
    update["message"]["entities"] = json!([{"type":"bot_command","offset":0,"length":5}]);
    let candidate = binding.pair(99, &update, now).unwrap();
    (code, candidate)
}
fn activate(binding: &mut Binding, now: Instant) -> ActiveBinding {
    let (_, candidate) = pair(binding, now);
    binding.confirm(candidate, target(), false, now).unwrap()
}
fn assert_inactive(binding: &Binding, generation: Uuid) {
    assert!(
        binding
            .authorize(99, generation, &message("private text"))
            .is_err()
    );
}

#[test]
fn nonce_only_nominates_and_exact_local_confirmation_activates() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let start = binding.snapshot(now);
    assert_eq!(start.phase, Phase::Unconfigured);
    assert!(!start.generation.is_nil());
    let (code, candidate) = pair(&mut binding, now);
    assert_eq!(code.expires_in, Duration::from_secs(600));
    assert_eq!(Uuid::parse_str(&code.nonce).unwrap().get_version_num(), 4);
    assert_ne!(code.generation, start.generation);
    assert_eq!(candidate.generation, code.generation);
    assert_eq!(candidate.bot_id, 99);
    assert_eq!(candidate.user_id, 101);
    assert_eq!(candidate.chat_id, 202);
    let snapshot = binding.snapshot(now);
    assert_eq!(snapshot.phase, Phase::AwaitingConfirmation);
    assert_eq!(snapshot.candidate, Some(candidate));
    assert!(snapshot.active.is_none());
    assert_inactive(&binding, code.generation);
    let public = serde_json::to_string(&snapshot).unwrap();
    assert!(!public.contains(&code.nonce));
    assert!(!format!("{snapshot:?}").contains(&code.nonce));
    let active = binding.confirm(candidate, target(), false, now).unwrap();
    assert_ne!(active.generation, code.generation);
    assert_eq!(binding.snapshot(now).phase, Phase::Active);
    assert!(matches!(
        binding.authorize(99, active.generation, &message("hello")),
        Ok(Inbound::Text { .. })
    ));
    assert_eq!(binding.revalidate(active.generation, target()), Ok(active));
    assert_eq!(
        binding.confirm(candidate, target(), false, now),
        Err(Failure::CandidateMismatch)
    );
}

#[test]
fn nonce_is_single_use_and_start_never_grants_access() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let code = binding.begin_pair(BotIdentity { id: 99 }, now).unwrap();
    assert_eq!(
        binding.pair(99, &message("/start"), now),
        Err(Failure::InvalidMessage)
    );
    assert_eq!(
        binding.pair(99, &message("/bind incorrect"), now),
        Err(Failure::InvalidNonce)
    );
    assert_eq!(
        binding.pair(98, &message(&format!("/bind {}", code.nonce)), now),
        Err(Failure::WrongBot)
    );
    assert_inactive(&binding, code.generation);
    let command = message(&format!("/bind {}", code.nonce));
    let candidate = binding.pair(99, &command, now).unwrap();
    assert_eq!(binding.pair(99, &command, now), Err(Failure::NotPairing));
    assert_eq!(binding.snapshot(now).candidate, Some(candidate));
}

#[test]
fn interactive_consent_is_explicit_and_never_inherited_by_a_new_generation() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let (_, candidate) = pair(&mut binding, now);
    let consenting = binding.confirm(candidate, target(), true, now).unwrap();
    assert!(consenting.interactive_consent);
    assert_ne!(consenting.generation, candidate.generation);
    assert_eq!(
        serde_json::to_value(binding.snapshot(now)).unwrap()["active"]["interactiveConsent"],
        true
    );
    // A consumed candidate cannot change consent in place.
    assert_eq!(
        binding.confirm(candidate, target(), false, now),
        Err(Failure::CandidateMismatch)
    );
    assert_eq!(binding.snapshot(now).active, Some(consenting));
    let (_, next_candidate) = pair(&mut binding, now);
    assert!(binding.snapshot(now).active.is_none());
    let text_only = binding
        .confirm(next_candidate, target(), false, now)
        .unwrap();
    assert!(!text_only.interactive_consent);
    assert_ne!(text_only.generation, consenting.generation);
    assert_ne!(text_only.generation, next_candidate.generation);
    assert_eq!(
        binding.revalidate(consenting.generation, target()),
        Err(Failure::StaleGeneration)
    );
    assert_eq!(
        serde_json::to_value(binding.snapshot(now)).unwrap()["active"]["interactiveConsent"],
        false
    );
    binding.revoke();
    assert!(binding.snapshot(now).active.is_none());
    assert!(!activate(&mut binding, now).interactive_consent);
    assert!(Binding::new().snapshot(now).active.is_none());
}

#[test]
fn original_deadline_applies_to_nonce_and_local_confirmation() {
    let now = Instant::now();
    for candidate_first in [false, true] {
        let mut binding = Binding::new();
        let code = binding.begin_pair(BotIdentity { id: 99 }, now).unwrap();
        let command = message(&format!("/bind {}", code.nonce));
        if candidate_first {
            let candidate = binding
                .pair(99, &command, now + PAIR_LIFETIME - Duration::from_secs(1))
                .unwrap();
            assert_eq!(
                binding.confirm(candidate, target(), false, now + PAIR_LIFETIME),
                Err(Failure::Expired)
            );
        } else {
            assert_eq!(
                binding.pair(99, &command, now + PAIR_LIFETIME),
                Err(Failure::Expired)
            );
        }
        let snapshot = binding.snapshot(now + PAIR_LIFETIME);
        assert_eq!(snapshot.phase, Phase::Expired);
        assert_ne!(snapshot.generation, code.generation);
        assert!(snapshot.candidate.is_none());
        assert!(snapshot.active.is_none());
        assert_inactive(&binding, code.generation);
    }
    // A caller passing an older monotonic sample must not extend the deadline.
    let mut binding = Binding::new();
    let code = binding
        .begin_pair(BotIdentity { id: 99 }, now + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        binding.pair(99, &message(&format!("/bind {}", code.nonce)), now),
        Err(Failure::Expired)
    );
}

#[test]
fn forwarded_edited_media_and_ambiguous_entities_cannot_nominate() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let code = binding.begin_pair(BotIdentity { id: 99 }, now).unwrap();
    let command = message(&format!("/bind {}", code.nonce));
    for field in [
        "forward_origin",
        "is_automatic_forward",
        "edit_date",
        "photo",
        "caption",
        "reply_to_message",
        "external_reply",
        "quote",
        "via_bot",
    ] {
        let mut update = command.clone();
        update["message"][field] = json!(true);
        assert_eq!(binding.pair(99, &update, now), Err(Failure::InvalidMessage));
    }
    let mut edited = json!({"update_id":1,"edited_message":command["message"].clone()});
    assert_eq!(binding.pair(99, &edited, now), Err(Failure::InvalidMessage));
    edited["message"] = command["message"].clone();
    assert_eq!(binding.pair(99, &edited, now), Err(Failure::InvalidMessage));
    for entities in [
        json!([]),
        json!([{"type":"text_link","offset":0,"length":5,"url":"https://example.invalid"}]),
        json!([{"type":"bot_command","offset":0,"length":6}]),
        json!([{"type":"bot_command","offset":0,"length":5,"unexpected":true}]),
        json!([{"type":"bot_command","offset":0,"length":5},{"type":"bold","offset":0,"length":5}]),
    ] {
        let mut update = command.clone();
        update["message"]["entities"] = entities;
        assert_eq!(binding.pair(99, &update, now), Err(Failure::InvalidMessage));
    }
    // Invalid attempts never consume the valid nonce.
    assert!(binding.pair(99, &command, now).is_ok());
}

#[test]
fn pair_requires_positive_private_nonbot_identity() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let code = binding.begin_pair(BotIdentity { id: 99 }, now).unwrap();
    let command = message(&format!("/bind {}", code.nonce));
    for id in [
        json!(0),
        json!(-1),
        json!("101"),
        Value::Null,
        json!(9223372036854775808_u64),
    ] {
        for field in ["from", "chat"] {
            let mut update = command.clone();
            update["message"][field]["id"] = id.clone();
            assert_eq!(
                binding.pair(99, &update, now),
                Err(Failure::InvalidIdentity)
            );
        }
    }
    let mut update = command.clone();
    update["message"]["chat"]["type"] = json!("group");
    assert_eq!(binding.pair(99, &update, now), Err(Failure::InvalidMessage));
    update = command.clone();
    update["message"]["from"]["is_bot"] = json!(true);
    assert_eq!(binding.pair(99, &update, now), Err(Failure::InvalidMessage));
    assert!(binding.pair(99, &command, now).is_ok());
}

#[test]
fn confirmation_requires_every_candidate_field_and_valid_explicit_target() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let (_, candidate) = pair(&mut binding, now);
    for altered in [
        Candidate {
            generation: Uuid::nil(),
            ..candidate
        },
        Candidate {
            bot_id: 100,
            ..candidate
        },
        Candidate {
            user_id: 102,
            ..candidate
        },
        Candidate {
            chat_id: 203,
            ..candidate
        },
    ] {
        assert_eq!(
            binding.confirm(altered, target(), false, now),
            Err(Failure::CandidateMismatch)
        );
    }
    for bad in [
        QualifiedTarget {
            run_epoch: Uuid::nil(),
            ..target()
        },
        QualifiedTarget {
            thread_id: Uuid::nil(),
            ..target()
        },
        QualifiedTarget {
            transport_epoch: Uuid::nil(),
            ..target()
        },
        QualifiedTarget {
            subscription_generation: Uuid::nil(),
            ..target()
        },
    ] {
        assert_eq!(
            QualifiedTarget::new(
                bad.run_epoch,
                bad.thread_id,
                bad.transport_epoch,
                bad.subscription_generation,
            ),
            Err(Failure::InvalidIdentity)
        );
        assert_eq!(
            binding.confirm(candidate, bad, false, now),
            Err(Failure::InvalidIdentity)
        );
    }
    assert!(binding.confirm(candidate, target(), false, now).is_ok());
}

#[test]
fn active_authorization_checks_bot_user_chat_generation_and_target() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let active = activate(&mut binding, now);
    assert!(matches!(
        binding.authorize(98, active.generation, &message("hello")),
        Err(Failure::WrongBot)
    ));
    for generation in [Uuid::nil(), Uuid::new_v4()] {
        assert!(matches!(
            binding.authorize(99, generation, &message("hello")),
            Err(Failure::StaleGeneration)
        ));
    }
    let mut wrong_user = message("hello");
    wrong_user["message"]["from"]["id"] = json!(102);
    assert!(matches!(
        binding.authorize(99, active.generation, &wrong_user),
        Ok(Inbound::Reject(telegram::Reject::UnauthorizedUser))
    ));
    let mut wrong_chat = message("hello");
    wrong_chat["message"]["chat"]["id"] = json!(203);
    assert!(matches!(
        binding.authorize(99, active.generation, &wrong_chat),
        Ok(Inbound::Reject(telegram::Reject::UnauthorizedChat))
    ));
    for other in [
        QualifiedTarget {
            run_epoch: Uuid::new_v4(),
            ..target()
        },
        QualifiedTarget {
            thread_id: Uuid::new_v4(),
            ..target()
        },
        QualifiedTarget {
            transport_epoch: Uuid::new_v4(),
            ..target()
        },
    ] {
        assert_eq!(
            binding.revalidate(active.generation, other),
            Err(Failure::WrongTarget)
        );
        assert!(!binding.invalidate(active.generation, Some(other)));
    }
    assert_eq!(binding.revalidate(active.generation, target()), Ok(active));
    // The pairing deadline is not an active session timeout.
    assert_eq!(binding.snapshot(now + PAIR_LIFETIME).phase, Phase::Active);
}

#[test]
fn revoke_bot_replacement_and_restart_never_restore_old_authorization() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let active = activate(&mut binding, now);
    binding.revoke();
    assert_inactive(&binding, active.generation);
    assert_eq!(binding.snapshot(now).phase, Phase::Idle);
    let active = activate(&mut binding, now);
    binding.replace_bot(BotIdentity { id: 100 }).unwrap();
    assert_inactive(&binding, active.generation);
    assert_eq!(binding.snapshot(now).bot_id, Some(100));
    let active = activate(&mut binding, now);
    let mut restarted = Binding::new();
    restarted.replace_bot(BotIdentity { id: 99 }).unwrap();
    assert_ne!(restarted.snapshot(now).generation, active.generation);
    assert_inactive(&restarted, active.generation);
    let (code, candidate) = pair(&mut binding, now);
    binding.revoke();
    assert_eq!(
        binding.pair(99, &message(&format!("/bind {}", code.nonce)), now),
        Err(Failure::NotPairing)
    );
    assert_eq!(
        binding.confirm(candidate, target(), false, now),
        Err(Failure::CandidateMismatch)
    );
    for id in [0, -1] {
        assert!(matches!(
            binding.begin_pair(BotIdentity { id }, now),
            Err(Failure::InvalidIdentity)
        ));
    }
}

#[test]
fn gap_unload_and_old_generation_invalidation_obey_exact_lifetime() {
    let now = Instant::now();
    let mut binding = Binding::new();
    let old = activate(&mut binding, now);
    assert!(binding.invalidate(old.generation, Some(old.target)));
    assert_inactive(&binding, old.generation);
    assert!(!binding.invalidate(old.generation, None));
    let new = activate(&mut binding, now);
    assert!(!binding.invalidate(old.generation, Some(old.target)));
    assert!(!binding.invalidate(Uuid::nil(), None));
    assert_eq!(binding.revalidate(new.generation, new.target), Ok(new));
    assert!(binding.invalidate(new.generation, None));
    assert_inactive(&binding, new.generation);
}
