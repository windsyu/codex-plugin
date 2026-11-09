use super::*;
use crate::workbench::redaction::RedactionPolicy;
use serde_json::{Value, json};

fn read(body: Value) -> ContextAnalysis {
    analyze(&body, &RedactionPolicy::new(vec![]).unwrap())
}

fn check_sums(analysis: &ContextAnalysis) {
    assert_eq!(
        analysis
            .summary
            .parts
            .iter()
            .map(|part| part.kind)
            .collect::<Vec<_>>(),
        KINDS
    );
    assert_eq!(
        analysis.summary.content_bytes,
        analysis.items.iter().map(|i| i.content_bytes).sum::<u64>()
    );
    assert_eq!(
        analysis.summary.estimated_tokens,
        analysis
            .items
            .iter()
            .map(|i| i.estimated_tokens)
            .sum::<u64>()
    );
    for part in &analysis.summary.parts {
        let items: Vec<_> = analysis
            .items
            .iter()
            .filter(|i| i.kind == part.kind)
            .collect();
        assert!(items.len() <= 24);
        assert_eq!(
            part.content_bytes,
            items.iter().map(|i| i.content_bytes).sum::<u64>()
        );
        assert_eq!(
            part.estimated_tokens,
            items.iter().map(|i| i.estimated_tokens).sum::<u64>()
        );
    }
}

#[test]
fn seven_classes_and_closed_envelopes_keep_mixed_user_tail() {
    let analysis = read(
        json!({"instructions":"base instructions", "tools":[{"type":"function","name":"read_file","parameters":{}}], "input":[
            {"role":"developer","content":"<skills_instructions>Read /a/SKILL.md</skills_instructions> developer tail"},
            {"role":"user","content":"# AGENTS.md instructions for /project\n<INSTRUCTIONS>Use tests.</INSTRUCTIONS>\n<environment_context><cwd>/project</cwd><shell>zsh</shell></environment_context>\nActual request"},
            {"type":"function_call","name":"read_file","call_id":"read-1","arguments":"{\"path\":\"a.rs\"}"},
            {"type":"function_call_output","call_id":"read-1","output":"file content"},
            {"type":"function_call_output","call_id":"unknown","output":"unassigned result"}
        ]}),
    );
    assert!(analysis.summary.coverage.complete);
    assert_eq!(
        analysis
            .summary
            .parts
            .iter()
            .map(|p| p.kind)
            .collect::<Vec<_>>(),
        KINDS
    );
    assert!(
        analysis
            .items
            .iter()
            .any(|i| i.kind == ContextKind::Memory && i.name == "/project/AGENTS.md")
    );
    let chat = analysis
        .items
        .iter()
        .find(|i| i.name == "消息 2" && i.kind == ContextKind::Chat)
        .unwrap();
    assert_eq!(chat.content_bytes, "Actual request".len() as u64);
    assert!(
        analysis
            .items
            .iter()
            .any(|i| i.name == "未归属工具结果" && i.kind == ContextKind::Results)
    );
    check_sums(&analysis);
}

#[test]
fn malformed_and_quoted_wrappers_are_not_hidden_and_skill_directory_ends() {
    let analysis = read(json!({"input":[
        {"role":"user","content":"<system-reminder>not closed"},
        {"role":"user","content":"Example: <skills>not an injected block</skills>"},
        {"role":"developer","content":"## Available skills\n- read: /a/SKILL.md\n## Next section\nkeep this system text"}
    ]}));
    assert_eq!(
        analysis
            .items
            .iter()
            .filter(|i| i.kind == ContextKind::Chat)
            .count(),
        2
    );
    assert!(analysis.items.iter().any(|i| i.kind == ContextKind::System
        && i.content_bytes == "## Next section\nkeep this system text".len() as u64));
    assert!(analysis.items.iter().any(|i| i.kind == ContextKind::Skills));
}

#[test]
fn namespace_and_additional_tools_reference_real_leaves_once() {
    let first = json!({"type":"function","name":"one","parameters":{"type":"object"}});
    let second = json!({"type":"function","name":"two","parameters":{"type":"object"}});
    let analysis = read(
        json!({"input":[{"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"mcp__search__","tools":[first,second]}]}]}),
    );
    assert!(analysis.summary.coverage.complete);
    let item = &analysis.items[0];
    assert_eq!(item.tag, "mcp");
    assert_eq!(item.occurrences, 2);
    assert_eq!(
        item.references,
        ["/input/0/tools/0/tools/0", "/input/0/tools/0/tools/1"]
    );
    assert_eq!(
        item.content_bytes,
        (first.to_string().len() + second.to_string().len()) as u64
    );
    check_sums(&analysis);
}

#[test]
fn read_output_uses_unique_identity_in_either_order_and_multi_file_group() {
    let analysis = read(json!({"input":[
        {"type":"function_call_output","call_id":"r","output":"contents of both files"},
        {"type":"function_call","call_id":"r","name":"exec_command","arguments":"{\"cmd\":\"cat 'a file.rs' b.rs\"}"},
        {"type":"function_call","call_id":"d","name":"read_file","arguments":"{\"path\":\"x\"}"},
        {"type":"function_call","call_id":"d","name":"read_file","arguments":"{\"path\":\"y\"}"},
        {"type":"function_call_output","call_id":"d","output":"duplicate call IDs"}
    ]}));
    let files: Vec<_> = analysis
        .items
        .iter()
        .filter(|i| i.kind == ContextKind::Files)
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "a file.rs · b.rs");
    assert_eq!(
        files[0].content_bytes,
        "contents of both files".len() as u64
    );
    assert!(
        analysis
            .items
            .iter()
            .any(|i| i.kind == ContextKind::Results && i.name == "未归属工具结果")
    );
}

#[test]
fn fixed_code_mode_is_read_but_complex_code_or_shell_is_not() {
    for (command, is_read) in [
        (
            "text(await tools.exec_command({cmd:\"sed -n '1,20p' a.rs\"}));",
            true,
        ),
        (
            "text(await tools.exec_command({cmd:\"cat a.rs | head\"}));",
            false,
        ),
        (
            "const cmd = 'cat a.rs'; text(await tools.exec_command({cmd}));",
            false,
        ),
        (
            "text(await tools.exec_command({cmd:\"cat a.rs\"})); text('other')",
            false,
        ),
    ] {
        let a = read(
            json!({"input":[{"type":"custom_tool_call","call_id":"c","name":"exec","input":command},{"type":"custom_tool_call_output","call_id":"c","output":"file text"}]}),
        );
        assert_eq!(
            a.items.iter().any(|i| i.kind == ContextKind::Files),
            is_read,
            "{command}"
        );
    }
}

#[test]
fn unknown_opaque_and_server_history_never_calibrate() {
    let a = read(
        json!({"previous_response_id":"previous","truncation":"auto","input":[
            {"type":"future_kind","payload":"hidden"},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"visible thought"}],"encrypted_content":"opaque cipher"},
            {"type":"compaction","encrypted_content":"more opaque"},
            {"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,opaque"}]}
        ]}),
    );
    assert!(!a.summary.coverage.complete);
    for code in [
        "unknown_content",
        "opaque_content",
        "server_history",
        "unsupported_settings",
    ] {
        assert!(a.summary.coverage.issues.iter().any(|i| i == code));
    }
    assert!(
        a.items
            .iter()
            .filter(|i| i.tag == "opaque" || i.tag == "unknown")
            .all(|i| i.estimated_tokens == 0 && i.content_bytes > 0)
    );
    assert!(!present(&a, Some(100)).calibrated);
    assert!(!serde_json::to_string(&a).unwrap().contains("opaque cipher"));
    check_sums(&a);
}

#[test]
fn original_long_content_is_measured_without_preview_or_redaction_bias() {
    let text = format!(
        "{}synthetic-secret{}",
        "中".repeat(20_000),
        "a".repeat(100_000)
    );
    let body = json!({"instructions":text,"tools":[{"type":"function","name":"synthetic-secret","description":"raw description"}]});
    let plain = read(body.clone());
    let masked = analyze(
        &body,
        &RedactionPolicy::new(vec!["synthetic-secret".into()]).unwrap(),
    );
    assert_eq!(plain.summary.content_bytes, masked.summary.content_bytes);
    assert_eq!(
        plain.summary.estimated_tokens,
        masked.summary.estimated_tokens
    );
    assert!(plain.summary.content_bytes > 128 * 1024);
    assert!(
        !serde_json::to_string(&masked)
            .unwrap()
            .contains("synthetic-secret")
    );
    assert!(
        !serde_json::to_string(&masked)
            .unwrap()
            .contains("raw description")
    );
}

#[test]
fn aggregation_and_reference_limits_preserve_weights_and_fit_budget() {
    let tools: Vec<_> = (0..100).map(|index| json!({"type":"function","name":format!("function{index}"),"description":"x".repeat(index+1)})).collect();
    let expected_bytes: u64 = tools.iter().map(|v| v.to_string().len() as u64).sum();
    let expected_tokens: u64 = tools
        .iter()
        .map(|v| (v.to_string().len() as u64).div_ceil(4))
        .sum();
    let a = read(json!({"tools":tools}));
    assert_eq!(a.summary.content_bytes, expected_bytes);
    assert_eq!(a.summary.estimated_tokens, expected_tokens);
    assert_eq!(a.items.len(), 24);
    assert_eq!(a.items.iter().map(|i| i.occurrences).sum::<u32>(), 100);
    assert!(
        a.items
            .iter()
            .any(|i| i.tag == "other" && i.references_partial)
    );
    assert!(serde_json::to_vec(&a).unwrap().len() <= 32 * 1024);
    assert!(a.summary.coverage.complete);
    check_sums(&a);
}

#[test]
fn repeated_content_counts_each_occurrence_and_schema_is_system() {
    let a = read(
        json!({"instructions":"same","input":[{"role":"developer","content":"same"},{"role":"developer","content":"same"}],"text":{"format":{"type":"json_schema","schema":{"type":"object"}}}}),
    );
    let repeated: u64 = a
        .items
        .iter()
        .filter(|i| i.tag == "system")
        .map(|i| i.content_bytes)
        .sum();
    assert_eq!(repeated, 12);
    assert!(
        a.items
            .iter()
            .any(|i| i.tag == "schema" && i.kind == ContextKind::System && i.references == [""])
    );
    assert!(a.summary.coverage.complete);
}

#[test]
fn calibration_has_exact_hierarchical_sums_for_zero_and_large_safe_counts() {
    let a = read(
        json!({"instructions":"a","input":[{"role":"user","content":"abc"},{"role":"assistant","content":"d"}],"tools":[{"type":"function","name":"a"},{"type":"function","name":"b"}]}),
    );
    for total in [0, 1, 2, 7, 1000, 9_007_199_254_740_991] {
        let p = present(&a, Some(total));
        assert!(p.calibrated);
        assert_eq!(p.tokens, total);
        assert_eq!(p.parts.iter().map(|i| i.tokens).sum::<u64>(), total);
        assert_eq!(p.items.iter().map(|i| i.tokens).sum::<u64>(), total);
        for part in &p.parts {
            assert_eq!(
                part.tokens,
                p.items
                    .iter()
                    .filter(|i| i.item.kind == part.part.kind)
                    .map(|i| i.tokens)
                    .sum::<u64>()
            );
        }
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::to_value(present(&a, Some(total))).unwrap()
        );
        assert_eq!(p.estimated_tokens, a.summary.estimated_tokens);
    }
    assert!(!present(&a, Some(9_007_199_254_740_992)).calibrated);
    assert!(!present(&a, None).calibrated);
    assert!(!present(&read(json!({"input":[]})), Some(0)).calibrated);
}

#[test]
fn bounded_analysis_and_json_contract_roundtrip() {
    let mut deeply_nested = json!("bottom");
    for _ in 0..70 {
        deeply_nested = json!([deeply_nested]);
    }
    let a = read(json!({"input":deeply_nested}));
    assert_eq!(a.summary.coverage.issues, ["budget_exceeded"]);
    let a = read(json!({"input":[{"role":"user","content":"hello"}]}));
    let value = serde_json::to_value(&a).unwrap();
    let recovered: ContextAnalysis = serde_json::from_value(value.clone()).unwrap();
    assert!(a == recovered);
    assert_eq!(value["summary"]["methodVersion"], 1);
    let presented = serde_json::to_value(present(&a, Some(42))).unwrap();
    let chat = presented["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["kind"] == "chat")
        .unwrap();
    assert_eq!(chat["tokens"], 42);
    assert!(presented["items"][0].get("item").is_none());
}

#[test]
fn metadata_budget_trims_navigation_without_losing_category_totals() {
    let mut input = Vec::new();
    for index in 0..60 {
        input.push(json!({"role":"user","content":format!("# AGENTS.md instructions for /project/{index}\n<INSTRUCTIONS>{}</INSTRUCTIONS>\nuser message {index}", "instructions ".repeat(10))}));
        input.push(json!({"type":"function_call","name":"read_file","call_id":format!("read{index}"),"arguments":json!({"path":format!("/project/{index}/{}", "a".repeat(110))}).to_string()}));
        input.push(json!({"type":"function_call_output","call_id":format!("read{index}"),"output":"file text"}));
        input.push(json!({"type":"function_call","name":format!("other_tool{index}"),"call_id":format!("other{index}"),"arguments":"{}"}));
        input.push(json!({"type":"function_call_output","call_id":format!("other{index}"),"output":"tool output"}));
    }
    let tools: Vec<_> = (0..60)
        .map(|index| json!({"type":"function","name":format!("definition{index}"),"parameters":{}}))
        .collect();
    let a = read(json!({"input":input,"tools":tools}));
    assert!(a.summary.coverage.complete);
    assert!(a.items.iter().any(|i| i.references_partial));
    assert!(serde_json::to_vec(&a).unwrap().len() <= 32 * 1024);
    check_sums(&a);
}

#[test]
fn unknown_fields_do_not_silently_qualify_for_calibration() {
    for input in [
        json!({"role":"user","content":"hi","new_context":"not understood"}),
        json!({"role":"user","content":[{"type":"input_text","text":"hi","new_context":"not understood"}]}),
        json!({"type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"not understood"}]}),
        json!({"type":"function_call_output","call_id":"c","output":"hi","new_context":"not understood"}),
        json!({"type":"function_call_output","call_id":"c","output":[{"type":"input_text","text":"hi","new_context":"not understood"}]}),
        json!({"type":"additional_tools","role":"developer","tools":[],"new_context":"not understood"}),
        json!({"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":[],"new_context":"not understood"}]}),
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":"hi","new_context":"not understood"}]}),
    ] {
        let a = read(json!({"input":[input]}));
        assert!(
            a.summary
                .coverage
                .issues
                .iter()
                .any(|i| i == "unknown_content")
        );
        assert!(!present(&a, Some(10)).calibrated);
    }
}
