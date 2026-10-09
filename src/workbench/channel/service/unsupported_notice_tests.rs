// Unsupported requests stay on the native path. Only an explicitly consented,
// currently owned channel task may emit the bounded, body-free local notice.
#[tokio::test]
async fn interaction_unsupported_notice_is_once_owned_consented_and_body_free() {
    for mode in ["owned", "foreign", "revoked", "unconsented"] {
        let mut f = Fixture::new(false).await;
        if mode != "unconsented" {
            f.bind_with_consent().await;
        }
        let (job, attempt, _cleanup) = f.owned_interaction_job().await;
        let before = f.journal();
        let mut request = command_interaction(json!("SYNTHETIC_UNSUPPORTED_RPC"));
        request["params"]["additionalPermissions"] = json!({"network":true});
        request["params"]["command"] = json!("SYNTHETIC_UNSUPPORTED_COMMAND");
        request["params"]["reason"] = json!("SYNTHETIC_UNSUPPORTED_REASON");
        request["params"]["cwd"] = json!("/private/SYNTHETIC_UNSUPPORTED_PATH");
        if mode == "foreign" {
            request["params"]["threadId"] = json!(Uuid::from_u128(999));
            request["params"]["turnId"] = json!("SYNTHETIC_FOREIGN_TURN");
        }
        // The helper verifies verbatim forwarding to the TUI and waits until
        // the Session has observed the wire; it does not fabricate authority.
        f.native_interaction(&attempt, request).await;
        if mode == "revoked" {
            f.service.revoke();
        }
        f.offer_interaction(&job, &attempt).await;
        let expected = if mode == "owned" {
            vec![json!({
                "chat_id":CHAT,
                "text":"A native interaction needs a local response. This channel cannot safely answer it.",
                "link_preview_options":{"is_disabled":true}
            })]
        } else {
            Vec::new()
        };
        assert_eq!(*f.bot.requests.lock().unwrap(), expected, "{mode}");
        f.offer_interaction(&job, &attempt).await;
        assert_eq!(*f.bot.requests.lock().unwrap(), expected, "{mode}");
        f.no_native_answer().await;
        assert!(f.service.core.inner.lock().unwrap().interactions.is_empty());
        assert_eq!(f.service.snapshot().queued, 0);
        assert_eq!(f.journal(), before, "a static notice cannot add a receipt");
        let outbound = serde_json::to_string(&*f.bot.requests.lock().unwrap()).unwrap();
        for private in [
            BODY,
            "SYNTHETIC_UNSUPPORTED_RPC",
            "SYNTHETIC_UNSUPPORTED_COMMAND",
            "SYNTHETIC_UNSUPPORTED_REASON",
            "SYNTHETIC_UNSUPPORTED_PATH",
            "SYNTHETIC_FOREIGN_TURN",
            "additionalPermissions",
        ] {
            assert!(!outbound.contains(private));
            assert!(!f.journal().contains(private));
        }
    }
}
