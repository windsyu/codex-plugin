//! Check installed CLI evidence before assigning human/model roles in the UI.
use super::*;
use crate::workbench::test_native::NativeProbe;
use std::io::BufRead;

const PROMPT: &str = "R1_CHAT_SOURCE：同一句真实提交，不使用工具。";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed official CLI and PTY; synthetic request/rollout identity only"]
async fn installed_cli_request_metadata_matches_native_user_events_and_separates_title_thread() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = seen.clone();
    let count = requests.clone();
    let upstream = fixture(move |request| {
        let observed = observed.clone(); let count = count.clone();
        async move {
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(request.into_body(), 2*1024*1024).await.unwrap()).unwrap();
            let client = &body["client_metadata"];
            let metadata: Value = serde_json::from_str(client["x-codex-turn-metadata"].as_str().expect("installed request missing canonical metadata")).unwrap();
            // Only the closed metadata shape is retained, never input/context.
            observed.lock().unwrap().push(json!({
                "thread": metadata["thread_id"], "turn": metadata["turn_id"],
                "source": metadata["thread_source"], "kind": metadata["request_kind"],
                "titleSchema": body.pointer("/text/format/schema/properties/title").is_some(),
                "model": body["model"],
                "flatAgrees": client["thread_id"] == metadata["thread_id"] && client["turn_id"] == metadata["turn_id"]
            }));
            if let Some(response) = title_response(&body) { return response; }
            let index = count.fetch_add(1, Ordering::SeqCst) + 1;
            interaction::final_message(index, &format!("R1_CHAT_SOURCE_REPLY_{index}"))
        }
    }).await;
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"model="gpt-6-astra"
model_provider="custom"
check_for_update_on_startup=false
[model_providers.custom]
name="Synthetic chat source probe"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
"#,
            upstream.address
        ),
    )
    .unwrap();
    let mut cli = NativeProbe::start(&home, &workspace, &[]).unwrap();
    cli.ready().await.unwrap();
    let mut folder_trust_confirmed = false;
    for index in 1..=2 {
        cli.submit(PROMPT).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                cli.pump().await.unwrap();
                let screen = cli.screen();
                // CLI 0.159.2 can defer folder trust until the first submission.
                // Confirm only this isolated fixture folder and never replay input.
                if !folder_trust_confirmed
                    && screen.contains("Folder access")
                    && screen.contains("Trust this folder?")
                {
                    assert_eq!(index, 1);
                    assert_eq!(requests.load(Ordering::SeqCst), 0);
                    folder_trust_confirmed = true;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    cli.write(b"\r").unwrap();
                    println!("{}", json!({"check":"deferred-native-folder-trust", "confirmed":true, "requestsBeforeConfirmation":0, "inputReplayed":false}));
                }
                if cli
                    .screen()
                    .contains(&format!("R1_CHAT_SOURCE_REPLY_{index}"))
                {
                    break;
                }
            }
        })
        .await;
        if reply.is_err() {
            let screen = cli.screen();
            println!(
                "{}",
                json!({"check":"native-chat-deadline", "submission":index,
                "conversationRequests": requests.load(Ordering::SeqCst),
                "observedRequests":seen.lock().unwrap().len(),
                "reply1":screen.contains("R1_CHAT_SOURCE_REPLY_1"),
                "reply2":screen.contains("R1_CHAT_SOURCE_REPLY_2"),
                "promptVisible":screen.contains(PROMPT),
                "modelNotice":screen.contains("Try new model"),
                "existingModel":screen.contains("Use existing model"),
                "error":screen.to_lowercase().contains("error"),
                "retry":screen.to_lowercase().contains("retry"),
                "unsupported":screen.to_lowercase().contains("not supported")})
            );
        }
        reply.expect("native reply deadline; raw screen suppressed");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    cli.quit().await.unwrap();
    let requests = seen.lock().unwrap().clone();
    println!(
        "{}",
        json!({"check":"native-request-kinds", "requests":requests.iter().map(|r| json!({"source":r["source"],"kind":r["kind"],"titleSchema":r["titleSchema"]})).collect::<Vec<_>>()})
    );
    assert!(requests.iter().all(|request| request["flatAgrees"] == true));
    let turns: Vec<_> = requests
        .iter()
        .filter(|request| request["source"] == "user" && request["kind"] == "turn")
        .collect();
    assert_eq!(turns.len(), 2, "exactly two native submissions");
    assert_eq!(turns[0]["thread"], turns[1]["thread"]);
    assert_ne!(turns[0]["turn"], turns[1]["turn"]);
    assert!(turns.iter().all(|request| request["kind"] == "turn"));
    let titles: Vec<_> = requests
        .iter()
        .filter(|request| request["titleSchema"] == true)
        .collect();
    assert!(titles.iter().all(|request| {
        matches!(request["source"].as_str(), Some("system" | "thread_title"))
            && request["kind"] == "turn"
    }));
    assert_eq!(
        requests.len(),
        turns.len() + titles.len(),
        "unclassified native request"
    );
    assert!(
        !titles.is_empty(),
        "title request needs independent source evidence"
    );
    assert!(
        titles
            .iter()
            .all(|request| request["thread"] != turns[0]["thread"])
    );
    let mut matches = Vec::new();
    let mut shapes = Vec::new();
    for entry in walkdir::WalkDir::new(home.join("sessions"))
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "jsonl")
        {
            continue;
        }
        let mut thread = Value::Null;
        let mut turn = Value::Null;
        let reader = std::io::BufReader::new(std::fs::File::open(entry.path()).unwrap());
        for line in reader.lines().map_while(Result::ok) {
            let value: Value = serde_json::from_str(&line).unwrap();
            let payload = &value["payload"];
            if value["type"] == "session_meta" {
                thread = payload["id"].clone();
            }
            if value["type"] == "event_msg" {
                shapes.push(payload["type"].clone());
                if matches!(
                    payload["type"].as_str(),
                    Some("task_started" | "turn_started")
                ) {
                    turn = payload["turn_id"].clone();
                }
                if payload["type"] == "user_message" && payload["message"] == PROMPT {
                    matches.push((thread.clone(), turn.clone()));
                }
                if payload["type"] == "item_completed" && payload["item"]["type"] == "UserMessage" {
                    let content = payload["item"]["content"].as_array().unwrap();
                    assert!(
                        content
                            .iter()
                            .any(|part| part["type"] == "text" && part["text"] == PROMPT)
                    );
                    assert_eq!(payload["thread_id"], thread);
                    assert_eq!(payload["turn_id"], turn);
                    assert!(
                        payload["item"]["id"]
                            .as_str()
                            .is_some_and(|id| !id.is_empty())
                    );
                    matches.push((thread.clone(), turn.clone()));
                }
            }
        }
    }
    assert_eq!(
        matches.len(),
        2,
        "identical submissions must retain two native user events"
    );
    for (index, (thread, turn)) in matches.iter().enumerate() {
        assert_eq!(*thread, turns[index]["thread"]);
        assert_eq!(*turn, turns[index]["turn"]);
    }
    println!(
        "{}",
        json!({"check":"native-chat-evidence","mainRequests":turns.len(),"mainSource":turns[0]["source"],"auxiliaryRequests":titles.len(),"userEvents":matches.len(),"flatMetadataAgrees":true,"nativeEventTypes":shapes})
    );
}
