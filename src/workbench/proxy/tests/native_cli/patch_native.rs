use super::*;
use crate::workbench::test_native::NativeProbe;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed CLI and PTY; synthetic failure/approval cancellation in temporary homes only"]
async fn installed_cli_file_change_failure_and_cancel_have_distinct_native_evidence() {
    for refused in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("blocker"), "R2 existing synthetic file\n").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let domain = Arc::new(Mutex::new(Value::Null));
        let observed = domain.clone();
        let upstream = fixture(move |request| {
            let (seen,observed) = (seen.clone(),observed.clone());
            async move {
                let body:Value=serde_json::from_slice(&axum::body::to_bytes(request.into_body(),2*1024*1024).await.unwrap()).unwrap();
                if let Some(title)=title_response(&body){return title;}
                let index=seen.fetch_add(1,Ordering::SeqCst)+1;
                if index == 1 {
                    assert!(tools_browser::advertises(&body,"apply_patch"));
                    *observed.lock().unwrap()=serde_json::from_str(body["client_metadata"]["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
                    let patch=format!("*** Begin Patch\n*** Add File: {}\n+R2 synthetic proposal\n*** End Patch",if refused {"refused.txt"} else {"blocker/child.txt"});
                    let item=json!({"id":"item_patch_native","call_id":"call_patch_native","type":"custom_tool_call","name":"apply_patch","input":patch});
                    let bytes=[event(json!({"type":"response.created","response":{"id":"resp_patch_native"}})),event(json!({"type":"response.output_item.done","output_index":0,"item":item})),event(json!({"type":"response.completed","response":{"id":"resp_patch_native"}}))].concat();
                    Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from(bytes)).unwrap()
                } else {
                    assert_eq!(index,2);
                    assert!(body["input"].as_array().unwrap().iter().any(|item|item["type"]=="custom_tool_call_output" && item["call_id"]=="call_patch_native"));
                    interaction::final_message(2,"R2_NATIVE_PATCH_RESULT_RECEIVED")
                }
            }
        }).await;
        std::fs::write(
            home.join("config.toml"),
            format!(
                r#"model="gpt-5.5"
model_provider="custom"
check_for_update_on_startup=false
approval_policy="{}"
approvals_reviewer="user"
sandbox_mode="{}"
[model_providers.custom]
name="Synthetic native patch facts"
base_url="http://{}/v1"
wire_api="responses"
requires_openai_auth=false
[features]
code_mode=false
code_mode_only=false
"#,
                if refused { "on-request" } else { "never" },
                if refused {
                    "read-only"
                } else {
                    "workspace-write"
                },
                upstream.address
            ),
        )
        .unwrap();
        let mut cli = NativeProbe::start(
            &home,
            &workspace,
            &["-c".into(), "tui.animations=false".into()],
        )
        .unwrap();
        cli.ready().await.unwrap();
        cli.submit("R2：验证原生修改结果。").await.unwrap();
        let mut rejected = false;
        let record = timeout(Duration::from_secs(20), async {
            loop {
                cli.pump().await.unwrap();
                if refused && !rejected && cli.screen().contains("Would you like") {
                    cli.write(b"\x1b").unwrap();
                    rejected = true;
                }
                if let Some(record) =
                    super::rollout_tools::native_events(&home)
                        .into_iter()
                        .find(|event| {
                            (refused && event["type"] == "turn_aborted")
                                || (event["item"]["type"] == "FileChange"
                                    && event["item"]["id"] == "call_patch_native")
                        })
                {
                    break record;
                }
            }
        })
        .await;
        let record = record.expect("native file change final evidence deadline");
        if refused {
            assert!(rejected);
            assert_eq!(record["type"], "turn_aborted");
            assert_eq!(record["turn_id"], domain.lock().unwrap()["turn_id"]);
            assert!(!workspace.join("refused.txt").exists());
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(
                !super::rollout_tools::native_items(&home)
                    .iter()
                    .any(|event| event["item"]["type"] == "FileChange")
            );
            println!(
                "{}",
                json!({"check":"native-file-change-cancel","turnAborted":true,"noToolFinalFact":true,"workspacePreserved":true})
            );
            cli.quit().await.unwrap();
            assert!(
                !super::rollout_tools::native_items(&home)
                    .iter()
                    .any(|event| event["item"]["type"] == "FileChange")
            );
            continue;
        }
        assert_eq!(record["thread_id"], domain.lock().unwrap()["thread_id"]);
        assert_eq!(record["turn_id"], domain.lock().unwrap()["turn_id"]);
        assert_eq!(record["item"]["status"], "failed");
        assert!(record["item"]["stdout"].is_string());
        assert!(record["item"]["stderr"].is_string());
        assert!(!workspace.join("refused.txt").exists());
        assert!(!workspace.join("blocker/child.txt").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("blocker")).unwrap(),
            "R2 existing synthetic file\n"
        );
        println!(
            "{}",
            json!({"check":"native-file-change-outcome","status":record["item"]["status"],"explicitThreadTurnCall":true,"stdoutPresent":true,"stderrPresent":true,"nativeRefusal":rejected,"workspacePreserved":true})
        );
        timeout(Duration::from_secs(5), async {
            while !cli.screen().contains("R2_NATIVE_PATCH_RESULT_RECEIVED") {
                cli.pump().await.unwrap();
            }
        })
        .await
        .unwrap();
        cli.quit().await.unwrap();
    }
}
