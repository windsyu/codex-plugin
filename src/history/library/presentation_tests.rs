use super::*;
use crate::history::library::model::{Source, sanitize};

const INSTRUCTIONS: &str = "# AGENTS.md instructions for /synthetic/project\n\n<INSTRUCTIONS>\nBuild carefully.\n</INSTRUCTIONS>";
const ENVIRONMENT: &str = "<environment_context>\n<cwd>/synthetic/project</cwd>\n<shell>zsh</shell>\n</environment_context>";

fn entry() -> Entry {
    Entry::new(
        &Source::Native {
            id: "test".into(),
            codex_home: "/synthetic/native".into(),
        },
        "thread",
    )
}
fn record(kind: &str, content: &str) -> Value {
    json!({"kind":kind,"text":content})
}

#[test]
fn wrappers_are_closed_leading_shapes_and_mixed_requests_survive() {
    for input in [
        INSTRUCTIONS.to_string(),
        ENVIRONMENT.to_string(),
        format!("{INSTRUCTIONS}\n{ENVIRONMENT}"),
    ] {
        assert_eq!(
            normalize(&record("user_message", &input))["presentation"],
            "context"
        );
        let mixed = format!("{input}\n\n请修复分组标题");
        let view = normalize(&record("user_message", &mixed));
        assert_eq!(view["presentation"], "conversation");
        assert_eq!(view["text"], "请修复分组标题");
    }
    for input in [
        "请解释 AGENTS.md 中的 <INSTRUCTIONS> 标记",
        "# AGENTS.md instructions for /synthetic/project\n<INSTRUCTIONS>missing close",
        "# AGENTS.md instructions for /synthetic/project\n<INSTRUCTIONS>nested <INSTRUCTIONS>x</INSTRUCTIONS>",
        "<environment_context>user authored XML</environment_context>",
        "<environment_context><cwd>/a</cwd><shell>zsh</environment_context>",
        "<environment_context><cwd>/a</cwd><shell>zsh</shell>please explain this XML</environment_context>",
        "<unknown_context>hello</unknown_context>",
        &format!("```xml\n{ENVIRONMENT}\n```"),
        &format!("> {INSTRUCTIONS}"),
        &format!("示例：\n{INSTRUCTIONS}"),
    ] {
        let view = normalize(&record("user_message", input));
        assert_eq!(view["presentation"], "conversation", "{input}");
        assert_eq!(view["text"], input);
    }
    // Assistant explanations of these wrappers remain answers.
    assert_eq!(
        normalize(&record("agent_message", INSTRUCTIONS))["presentation"],
        "conversation"
    );
    assert_eq!(
        normalize(&record(
            "user_message",
            "<external_codex_apps_open_page>{\"page_id\":null}</external_codex_apps_open_page>"
        ))["presentation"],
        "context"
    );
    for body in [
        "{\"page_id\":1}",
        "{\"page_id\":null,\"other\":true}",
        "{\"page_id\":1,\"page_id\":null}",
        "invalid",
    ] {
        let input =
            format!("<external_codex_apps_open_page>{body}</external_codex_apps_open_page>");
        assert_eq!(
            normalize(&record("user_message", &input))["presentation"],
            "conversation"
        );
    }
}

#[test]
fn confirmed_nested_environment_follows_agents_without_swallowing_real_request() {
    let filesystem = "<filesystem><workspace_roots><root>/synthetic/project</root></workspace_roots><permission_profile type=\"managed\"><file_system type=\"restricted\"><entry access=\"read\"><special>:root</special></entry><entry access=\"write\"><path>/synthetic/project</path></entry></file_system></permission_profile></filesystem>";
    let subagents = "<subagents><agent name=\"/root/test_worker\" /></subagents>";
    let environment = format!(
        "<environment_context><cwd>/synthetic/project</cwd><shell>zsh</shell><current_date>2026-10-08</current_date><timezone>Asia/Shanghai</timezone>{filesystem}{subagents}</environment_context>"
    );
    let context = format!("{INSTRUCTIONS}\n{environment}");
    let mut e = entry();
    observe(&mut e, &record("user_message", &context));
    assert_eq!(e.title, "未命名会话");
    assert!(e.preview.is_none());
    assert_eq!(
        normalize(&record("user_message", &context))["presentation"],
        "context"
    );
    let mixed = format!("{context}\n真正要完成的任务");
    observe(&mut e, &record("user_message", &mixed));
    assert_eq!(e.title, "真正要完成的任务");
    assert_eq!(
        normalize(&record("user_message", &mixed))["text"],
        "真正要完成的任务"
    );
    for invalid in [
        environment.replace("</entry>", ""),
        environment.replace("access=\"write\"", "access=\"future\""),
        environment.replace("<subagents>", "<subagents>real prose"),
        environment.replace(
            "<root>/synthetic/project</root>",
            "<future>unknown</future>",
        ),
        environment.replace(
            "name=\"/root/test_worker\"",
            "name=\"/root/test_worker\" other=\"x\"",
        ),
    ] {
        assert_eq!(
            normalize(&record("user_message", &invalid))["presentation"],
            "conversation"
        );
        assert_eq!(
            normalize(&record("user_message", &invalid))["text"],
            invalid
        );
    }
}

#[test]
fn metadata_has_no_fabricated_title_and_preserves_valid_authored_title() {
    let mut e = entry();
    observe(&mut e, &record("user_message", INSTRUCTIONS));
    observe(&mut e, &record("tool_output", "not a request"));
    assert_eq!(e.title, "未命名会话");
    assert!(e.preview.is_none());
    e.title = stored_title("用户命名的发布任务", &[0; 32]).unwrap();
    observe(&mut e, &record("user_message", "another request"));
    observe(&mut e, &record("agent_message", "last answer"));
    for kind in ["context", "reasoning", "unknown", "usage", "tool_output"] {
        observe(&mut e, &record(kind, "internal noise"));
    }
    assert_eq!(e.title, "用户命名的发布任务");
    assert_eq!(e.preview.unwrap().text, "last answer");
    assert!(stored_title(INSTRUCTIONS, &[0; 32]).is_none());
    assert!(stored_title(" \n ", &[0; 32]).is_none());
    let reasoning = json!({"kind":"agent_message","raw":{"payload":{"type":"message","role":"assistant","channel":"analysis","content":[{"text":"internal reasoning"}]}}});
    assert_eq!(normalize(&reasoning)["presentation"], "activity");
}

#[test]
fn all_sources_share_presentation_and_keep_raw_unchanged() {
    for (role, kind) in [("user", "userMessage"), ("assistant", "agentMessage")] {
        let content = if role == "user" {
            format!("{INSTRUCTIONS}\nreal request")
        } else {
            "real answer".into()
        };
        let native = json!({"kind":if role=="user"{"user_message"}else{"agent_message"},"text":"truncated summary","raw":{"payload":{"type":"message","role":role,"content":[{"text":content}]}}});
        let observer =
            json!({"kind":"observer_item","raw":{"itemType":kind,"summaryText":content}});
        let workbench = json!({"kind":"items","raw":{"kind":"message","author":{"role":role},"content":[{"text":content}]}});
        for source in [native, observer, workbench] {
            let before = source.clone();
            let projected = normalize(&source);
            assert_eq!(projected["presentation"], "conversation");
            assert_eq!(projected["role"], role);
            assert_eq!(
                projected["text"],
                if role == "user" {
                    "real request"
                } else {
                    "real answer"
                }
            );
            let mut e = entry();
            observe(&mut e, &source);
            assert_eq!(e.preview.unwrap().role, role);
            assert_eq!(before, source);
        }
    }
    for kind in ["tool_call", "unknown", "usage", "reasoning"] {
        assert_eq!(normalize(&record(kind, "data"))["presentation"], "activity");
    }
    for kind in ["error", "history_gap"] {
        assert_eq!(
            normalize(&record(kind, "problem"))["presentation"],
            "attention"
        );
    }
    assert_eq!(
        normalize(
            &json!({"kind":"items","raw":{"kind":"notice","code":"capture_gap","text":"captured history is incomplete"}})
        )["presentation"],
        "attention"
    );
}

#[test]
fn metadata_is_sanitized_then_unicode_bounded() {
    let mut e = entry();
    let safe = sanitize(
        &record(
            "user_message",
            &format!("token=sk-secretvalue1234567890 {}", "汉".repeat(300)),
        ),
        &[0; 32],
    );
    observe(&mut e, &safe);
    assert!(e.title.chars().count() <= 120);
    assert!(e.preview.as_ref().unwrap().text.chars().count() <= 240);
    assert!(!e.title.contains("sk-secretvalue1234567890"));
    assert!(!e.preview.unwrap().text.contains("sk-secretvalue1234567890"));
}
