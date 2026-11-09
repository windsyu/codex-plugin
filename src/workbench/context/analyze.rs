//! Semantic request classification before preview clipping. Never retains payload text.
// Inspired by magpie prompt.go, 1c45645b59e944b0446772a6786f1a31dd410a80.
// MIT, Copyright (c) 2026 yetone. See THIRD_PARTY_NOTICES.md.
use super::*;
use crate::context_envelope::{self, EnvelopeKind};
use crate::workbench::{decode::tool, redaction::RedactionPolicy};
use serde_json::Value;
use std::{collections::HashMap, io, sync::Arc};

const MAX_NODES: usize = 8192;
const MAX_ITEMS: usize = 4096;
const MAX_REFERENCES: usize = 4;
const ANALYSIS_BYTES: usize = 32 * 1024;
const NAME_BYTES: usize = 96;

fn text_tokens(text: &str) -> u64 {
    let ascii = text.bytes().filter(u8::is_ascii).count() as u64;
    ascii.div_ceil(4) + text.chars().filter(|c| !c.is_ascii()).count() as u64
}

// Counts compact JSON without allocating a second copy of schemas or opaque data.
#[derive(Default)]
struct ByteCount(u64);
impl io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn json_bytes(value: &Value) -> u64 {
    let mut count = ByteCount::default();
    serde_json::to_writer(&mut count, value).expect("JSON value serialization");
    count.0
}

fn extra_fields(value: &Value, known: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().any(|key| !known.contains(&key.as_str())))
}

fn within_budget(body: &Value) -> bool {
    let mut stack = vec![(body, 0)];
    let mut nodes = 0;
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES || depth > 12 {
            return false;
        }
        match value {
            Value::Array(values) => {
                if nodes + stack.len() + values.len() > MAX_NODES {
                    return false;
                }
                stack.extend(values.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(values) => {
                if nodes + stack.len() + values.len() > MAX_NODES {
                    return false;
                }
                stack.extend(values.values().map(|v| (v, depth + 1)));
            }
            _ => {}
        }
    }
    true
}

struct Builder<'a> {
    policy: &'a Arc<RedactionPolicy>,
    items: Vec<ContextItem>,
    // Raw keys only live during classification; output IDs are ordinal, never hashes of secrets.
    groups: HashMap<(ContextKind, String, &'static str), usize>,
    issues: Vec<String>,
}
impl<'a> Builder<'a> {
    fn new(policy: &'a Arc<RedactionPolicy>) -> Self {
        Self {
            policy,
            items: Vec::new(),
            groups: HashMap::new(),
            issues: Vec::new(),
        }
    }
    fn issue(&mut self, code: &'static str) {
        if !self.issues.iter().any(|s| s == code) {
            self.issues.push(code.into());
        }
    }
    fn add(
        &mut self,
        kind: ContextKind,
        name: &str,
        tag: &'static str,
        bytes: u64,
        tokens: u64,
        reference: &str,
    ) {
        let key = (kind, name.to_owned(), tag);
        let index = if let Some(index) = self.groups.get(&key) {
            *index
        } else {
            // More distinct names retain weights in a bounded category bucket.
            if self.items.len() >= MAX_ITEMS {
                self.issue("budget_exceeded");
                let key = (kind, String::new(), "overflow");
                if let Some(index) = self.groups.get(&key) {
                    *index
                } else {
                    let index = self.items.len();
                    self.items.push(ContextItem {
                        id: format!("c{index}"),
                        kind,
                        name: "其他内容".into(),
                        tag: "other".into(),
                        occurrences: 0,
                        content_bytes: 0,
                        estimated_tokens: 0,
                        references: Vec::new(),
                        references_partial: true,
                    });
                    self.groups.insert(key, index);
                    index
                }
            } else {
                let index = self.items.len();
                let safe = self.policy.scrub_tool(name);
                let name = safe.bounded(NAME_BYTES).as_str().to_owned();
                self.items.push(ContextItem {
                    id: format!("c{index}"),
                    kind,
                    name,
                    tag: tag.into(),
                    occurrences: 0,
                    content_bytes: 0,
                    estimated_tokens: 0,
                    references: Vec::new(),
                    references_partial: false,
                });
                self.groups.insert(key, index);
                index
            }
        };
        let item = &mut self.items[index];
        item.occurrences += 1;
        item.content_bytes += bytes;
        item.estimated_tokens += tokens;
        if !item.references.iter().any(|r| r == reference) {
            if item.references.len() < MAX_REFERENCES {
                item.references.push(reference.into());
            } else {
                item.references_partial = true;
            }
        }
    }
    fn text(
        &mut self,
        kind: ContextKind,
        name: &str,
        tag: &'static str,
        text: &str,
        reference: &str,
    ) {
        if !text.is_empty() {
            self.add(
                kind,
                name,
                tag,
                text.len() as u64,
                text_tokens(text),
                reference,
            );
        }
    }
    fn structured(
        &mut self,
        kind: ContextKind,
        name: &str,
        tag: &'static str,
        value: &Value,
        reference: &str,
    ) {
        let bytes = json_bytes(value);
        self.add(kind, name, tag, bytes, bytes.div_ceil(4), reference);
    }
    fn opaque(&mut self, kind: ContextKind, name: &str, value: &Value, reference: &str) {
        self.issue("opaque_content");
        self.add(kind, name, "opaque", json_bytes(value), 0, reference);
    }
    fn unknown(&mut self, kind: ContextKind, value: &Value, reference: &str) {
        self.issue("unknown_content");
        self.add(
            kind,
            "未识别内容",
            "unknown",
            json_bytes(value),
            0,
            reference,
        );
    }
    fn prose(&mut self, text: &str, system: bool, reference: &str, name: &str) {
        let mut remaining = text;
        while !remaining.is_empty() {
            if let Some(envelope) = context_envelope::leading(remaining) {
                let (kind, label, tag) = match envelope.kind {
                    EnvelopeKind::Memory => (
                        ContextKind::Memory,
                        format!("{}/AGENTS.md", envelope.name.trim_end_matches('/')),
                        "project",
                    ),
                    EnvelopeKind::Environment => {
                        (ContextKind::System, "环境上下文".into(), "environment")
                    }
                    EnvelopeKind::Notice => (ContextKind::System, "上下文通知".into(), "notice"),
                };
                self.text(kind, &label, tag, envelope.text, reference);
                remaining = envelope.tail;
                continue;
            }
            let trimmed = remaining.trim_start();
            let wrapper = [
                "skills_instructions",
                "skills",
                "system-reminder",
                "user_instructions",
            ]
            .into_iter()
            .find_map(|tag| {
                context_envelope::closed_block(trimmed, tag).map(|(_, tail)| (tag, tail))
            });
            if let Some((tag, tail)) = wrapper {
                let (kind, label, tag) = match tag {
                    "skills" | "skills_instructions" => {
                        (ContextKind::Skills, "技能目录与说明", "skills")
                    }
                    "system-reminder" => (ContextKind::Memory, "系统提醒", "reminder"),
                    _ => (ContextKind::System, "用户指令包装", "harness"),
                };
                self.text(
                    kind,
                    label,
                    tag,
                    &remaining[..remaining.len() - tail.len()],
                    reference,
                );
                remaining = tail;
                continue;
            }
            // Explicit skill directory headings only, never a mention of the word "skill".
            if let Some((skills, tail)) = skill_directory(remaining) {
                self.text(ContextKind::Skills, "技能目录", "skills", skills, reference);
                remaining = tail;
                continue;
            }
            self.text(
                if system {
                    ContextKind::System
                } else {
                    ContextKind::Chat
                },
                name,
                if system { "system" } else { "message" },
                remaining,
                reference,
            );
            break;
        }
    }
    fn tools(
        &mut self,
        values: &Value,
        reference: &str,
        group: Option<(&str, &'static str)>,
        depth: usize,
    ) {
        if depth > 8 {
            self.issue("budget_exceeded");
            return;
        }
        let Some(tools) = values.as_array() else {
            self.unknown(ContextKind::Tools, values, reference);
            return;
        };
        for (index, definition) in tools.iter().enumerate() {
            let pointer = format!("{reference}/{index}");
            let name = definition["name"].as_str().unwrap_or("工具定义");
            if definition["type"] == "namespace" && definition["tools"].is_array() {
                if extra_fields(definition, &["type", "name", "description", "tools"]) {
                    self.issue("unknown_content");
                }
                let own_group = if name == "functions" {
                    group
                } else if let Some(server) = mcp_group(name) {
                    Some((server, "mcp"))
                } else {
                    Some((name, "namespace"))
                };
                // Namespace's own description is content too. Its child array is
                // counted only below, keeping parent and child fragments disjoint.
                if let Some(description) = definition["description"].as_str() {
                    let (label, tag) = own_group.unwrap_or((name, "namespace"));
                    self.text(ContextKind::Tools, label, tag, description, &pointer);
                }
                self.tools(
                    &definition["tools"],
                    &format!("{pointer}/tools"),
                    own_group,
                    depth + 1,
                );
                continue;
            }
            let known = matches!(
                definition["type"].as_str(),
                Some(
                    "function"
                        | "custom"
                        | "web_search"
                        | "web_search_preview"
                        | "file_search"
                        | "code_interpreter"
                        | "computer"
                        | "computer_use_preview"
                        | "image_generation"
                        | "tool_search"
                        | "mcp"
                        | "shell"
                        | "local_shell"
                        | "apply_patch"
                )
            );
            if !known {
                self.unknown(ContextKind::Tools, definition, &pointer);
                continue;
            }
            let mcp = mcp_group(name);
            let (label, tag) = group
                .or(mcp.map(|server| (server, "mcp")))
                .unwrap_or((name, "tool"));
            self.structured(ContextKind::Tools, label, tag, definition, &pointer);
        }
    }
    fn content(&mut self, value: &Value, role: &str, reference: &str, name: &str) {
        if let Some(text) = value.as_str() {
            self.message_text(text, role, reference, name);
            return;
        }
        let Some(parts) = value.as_array() else {
            self.unknown(ContextKind::Chat, value, reference);
            return;
        };
        for part in parts {
            match part["type"].as_str() {
                Some("input_text" | "output_text" | "text") => {
                    if extra_fields(part, &["type", "text", "annotations", "logprobs"]) {
                        self.issue("unknown_content");
                    }
                    if let Some(text) = part["text"].as_str() {
                        self.message_text(text, role, reference, name);
                    } else {
                        self.unknown(ContextKind::Chat, part, reference);
                    }
                }
                Some("input_image" | "input_file" | "input_audio" | "image" | "audio") => {
                    self.opaque(ContextKind::Files, "媒体或附件", part, reference)
                }
                Some("refusal") if part["refusal"].is_string() => self.text(
                    ContextKind::Chat,
                    name,
                    "message",
                    part["refusal"].as_str().unwrap(),
                    reference,
                ),
                _ => self.unknown(ContextKind::Chat, part, reference),
            }
        }
    }
    fn message_text(&mut self, text: &str, role: &str, reference: &str, name: &str) {
        if role == "assistant" {
            self.text(ContextKind::Chat, name, "message", text, reference);
        } else {
            self.prose(
                text,
                matches!(role, "system" | "developer"),
                reference,
                name,
            );
        }
    }
    fn output(&mut self, value: &Value, call: Option<&Call>, reference: &str) {
        let (kind, name, tag) = match call {
            Some(Call {
                label,
                files: Some(_),
                ..
            }) => (ContextKind::Files, label.as_str(), "read"),
            Some(call) => (ContextKind::Results, call.label.as_str(), "result"),
            None => (ContextKind::Results, "未归属工具结果", "result"),
        };
        if let Some(text) = value.as_str() {
            self.text(kind, name, tag, text, reference);
        } else if let Some(parts) = value.as_array() {
            for part in parts {
                match part["type"].as_str() {
                    Some("input_text" | "text") if part["text"].is_string() => {
                        if extra_fields(part, &["type", "text"]) {
                            self.issue("unknown_content");
                        }
                        self.text(kind, name, tag, part["text"].as_str().unwrap(), reference)
                    }
                    Some("input_image" | "input_file" | "image" | "audio" | "input_audio") => {
                        self.opaque(ContextKind::Files, "工具返回媒体", part, reference)
                    }
                    _ => self.unknown(ContextKind::Results, part, reference),
                }
            }
        } else {
            self.unknown(ContextKind::Results, value, reference);
        }
    }
    fn finish(mut self) -> ContextAnalysis {
        let mut parts = Vec::new();
        let mut items = Vec::new();
        for kind in KINDS {
            let mut group: Vec<_> = self
                .items
                .iter()
                .filter(|item| item.kind == kind)
                .cloned()
                .collect();
            parts.push(ContextPartSummary {
                kind,
                content_bytes: group.iter().map(|i| i.content_bytes).sum(),
                estimated_tokens: group.iter().map(|i| i.estimated_tokens).sum(),
                item_count: group.len() as u32,
            });
            group.sort_by(|a, b| {
                b.estimated_tokens
                    .cmp(&a.estimated_tokens)
                    .then(a.id.cmp(&b.id))
            });
            if group.len() > 24 {
                let rest = group.split_off(23);
                group.push(merge_other(kind, rest));
            }
            items.extend(group);
        }
        self.issues.sort();
        let mut result = ContextAnalysis {
            summary: ContextSummary {
                schema_version: 1,
                method_version: 1,
                content_bytes: parts.iter().map(|p| p.content_bytes).sum(),
                estimated_tokens: parts.iter().map(|p| p.estimated_tokens).sum(),
                parts,
                coverage: ContextCoverage {
                    complete: self.issues.is_empty(),
                    issues: self.issues,
                },
            },
            items,
        };
        // References are a navigation aid, not the measurements. Removing them
        // doesn't make an otherwise complete classification partial.
        while serde_json::to_vec(&result).unwrap().len() > ANALYSIS_BYTES {
            if let Some(item) = result
                .items
                .iter_mut()
                .rev()
                .find(|i| !i.references.is_empty())
            {
                item.references.pop();
                item.references_partial = true;
            } else {
                let kind = KINDS
                    .into_iter()
                    .max_by_key(|kind| result.items.iter().filter(|i| i.kind == *kind).count())
                    .unwrap();
                let indexes: Vec<_> = result
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| i.kind == kind)
                    .map(|(n, _)| n)
                    .collect();
                if indexes.len() < 2 {
                    break;
                }
                let right = result.items.remove(*indexes.last().unwrap());
                let left_index = indexes[indexes.len() - 2];
                let left = result.items.remove(left_index);
                result
                    .items
                    .insert(left_index, merge_other(kind, vec![left, right]));
            }
        }
        result
    }
}

fn merge_other(kind: ContextKind, items: Vec<ContextItem>) -> ContextItem {
    let mut out = ContextItem {
        id: format!("other-{}", kind as u8),
        kind,
        name: "其他项".into(),
        tag: "other".into(),
        occurrences: 0,
        content_bytes: 0,
        estimated_tokens: 0,
        references: Vec::new(),
        references_partial: false,
    };
    for item in items {
        out.occurrences += item.occurrences;
        out.content_bytes += item.content_bytes;
        out.estimated_tokens += item.estimated_tokens;
        out.references_partial |= item.references_partial;
        for reference in item.references {
            if out.references.contains(&reference) {
                continue;
            }
            if out.references.len() < MAX_REFERENCES {
                out.references.push(reference);
            } else {
                out.references_partial = true;
            }
        }
    }
    out
}

fn mcp_group(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("mcp__")?;
    let server = rest.split("__").next()?;
    (!server.is_empty()).then_some(server)
}

fn skill_directory(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start();
    let first = trimmed.lines().next()?;
    if !matches!(
        first.trim(),
        "## Skills" | "# Skills" | "### Available skills" | "## Available skills"
    ) {
        return None;
    }
    let body = &trimmed[first.len()..];
    if !body.contains("SKILL.md") {
        return None;
    }
    let depth = first.bytes().take_while(|b| *b == b'#').count();
    let mut offset = first.len();
    for line in body.split_inclusive('\n') {
        let heading = line.bytes().take_while(|b| *b == b'#').count();
        if heading > 0 && heading <= depth && line.as_bytes().get(heading) == Some(&b' ') {
            let end = text.len() - trimmed.len() + offset;
            return Some((&text[..end], &text[end..]));
        }
        offset += line.len();
    }
    Some((text, ""))
}

struct Call {
    label: String,
    files: Option<Vec<String>>,
    kind: tool::ToolKind,
}

fn calls(items: &[Value], policy: &Arc<RedactionPolicy>) -> HashMap<String, Option<Call>> {
    let mut calls = HashMap::new();
    for item in items {
        let Some(kind) = tool::kind(item) else {
            continue;
        };
        let identity = tool::identity(item, kind, policy);
        let Some(id) = identity.call_id else {
            continue;
        };
        let known = !identity.invalid_fields && identity.name.is_some();
        let call = if known {
            let name = identity.name.unwrap();
            let native = identity
                .namespace
                .as_deref()
                .is_none_or(|ns| ns == "functions");
            let files = native
                .then(|| tool::arguments(item, kind).and_then(|raw| read_files(&name, raw, kind)))
                .flatten();
            let label = if let Some(files) = &files {
                files.join(" · ")
            } else {
                name
            };
            Some(Call { label, files, kind })
        } else {
            None
        };
        calls
            .entry(id)
            .and_modify(|old| *old = None)
            .or_insert(call);
    }
    calls
}

pub(crate) fn analyze(body: &Value, policy: &Arc<RedactionPolicy>) -> ContextAnalysis {
    let mut b = Builder::new(policy);
    if !within_budget(body) {
        b.issue("budget_exceeded");
        return b.finish();
    }
    let Some(object) = body.as_object() else {
        b.issue("unknown_content");
        return b.finish();
    };
    if let Some(previous) = body.get("previous_response_id")
        && !previous.is_null()
        && previous.as_str() != Some("")
    {
        b.issue("server_history");
    }
    if let Some(truncation) = body.get("truncation")
        && !truncation.is_null()
        && truncation != "disabled"
    {
        b.issue("unsupported_settings");
    }
    const SETTINGS: &[&str] = &[
        "model",
        "previous_response_id",
        "tool_choice",
        "parallel_tool_calls",
        "reasoning",
        "text",
        "max_output_tokens",
        "temperature",
        "top_p",
        "store",
        "stream",
        "service_tier",
        "truncation",
        "instructions",
        "input",
        "tools",
        "type",
        "client_metadata",
        "metadata",
        "include",
        "prompt_cache_key",
        "prompt_cache_retention",
        "safety_identifier",
    ];
    if object.keys().any(|key| !SETTINGS.contains(&key.as_str())) {
        b.issue("unsupported_settings");
    }
    if let Some(text) = body.get("instructions") {
        if let Some(text) = text.as_str() {
            b.prose(text, true, "/instructions", "系统说明");
        } else if !text.is_null() {
            b.unknown(ContextKind::System, text, "/instructions");
        }
    }
    if let Some(text) = body.get("text").filter(|v| !v.is_null()) {
        if text["format"]["type"] == "json_schema" && text["format"]["schema"].is_object() {
            b.structured(
                ContextKind::System,
                "输出 JSON Schema",
                "schema",
                &text["format"],
                "",
            );
        } else if !text.is_object()
            || text.get("format").is_some_and(|format| {
                !matches!(format["type"].as_str(), Some("text" | "json_object"))
            })
        {
            b.issue("unsupported_settings");
        }
    }
    if let Some(tools) = body.get("tools").filter(|v| !v.is_null()) {
        b.tools(tools, "/tools", None, 0);
    }
    let input = &body["input"];
    if let Some(text) = input.as_str() {
        // Existing details uses /input for its scalar input entry.
        b.prose(text, false, "/input", "用户消息");
    } else if let Some(items) = input.as_array() {
        let calls = calls(items, policy);
        for (index, item) in items.iter().enumerate() {
            let reference = format!("/input/{index}");
            let name = format!("消息 {}", index + 1);
            let kind = item["type"]
                .as_str()
                .unwrap_or(if item["role"].is_string() {
                    "message"
                } else {
                    ""
                });
            if let Some(call_kind) = tool::kind(item) {
                if extra_fields(
                    item,
                    &[
                        "type",
                        "id",
                        "call_id",
                        "name",
                        "namespace",
                        "arguments",
                        "input",
                        "status",
                    ],
                ) {
                    b.issue("unknown_content");
                }
                if let Some(arguments) = tool::arguments(item, call_kind) {
                    if call_kind == tool::ToolKind::Function {
                        if let Ok(arguments) = serde_json::from_str::<Value>(arguments) {
                            if within_budget(&arguments) {
                                b.structured(
                                    ContextKind::Chat,
                                    &name,
                                    "call",
                                    &arguments,
                                    &reference,
                                );
                            } else {
                                b.issue("budget_exceeded");
                            }
                        } else {
                            b.unknown(ContextKind::Chat, item, &reference);
                        }
                    } else {
                        b.text(ContextKind::Chat, &name, "call", arguments, &reference);
                    }
                } else {
                    b.unknown(ContextKind::Chat, item, &reference);
                }
                continue;
            }
            match kind {
                "message" => {
                    if extra_fields(item, &["type", "id", "role", "content", "status", "phase"]) {
                        b.issue("unknown_content");
                    }
                    let role = item["role"].as_str().unwrap_or("");
                    if !matches!(role, "system" | "developer" | "user" | "assistant") {
                        b.unknown(ContextKind::Chat, item, &reference);
                    } else {
                        b.content(&item["content"], role, &reference, &name);
                    }
                }
                "additional_tools" if item["role"] == "developer" => {
                    if extra_fields(item, &["type", "role", "id", "tools"]) {
                        b.issue("unknown_content");
                    }
                    b.tools(&item["tools"], &format!("{reference}/tools"), None, 0)
                }
                "function_call_output" | "custom_tool_call_output" => {
                    if extra_fields(item, &["type", "id", "call_id", "output", "status"]) {
                        b.issue("unknown_content");
                    }
                    let output_kind = if kind == "function_call_output" {
                        tool::ToolKind::Function
                    } else {
                        tool::ToolKind::Custom
                    };
                    let call = tool::identifier(&item["call_id"], policy)
                        .and_then(|id| calls.get(&id))
                        .and_then(Option::as_ref)
                        .filter(|call| call.kind == output_kind);
                    b.output(&item["output"], call, &reference);
                }
                "reasoning" => {
                    if extra_fields(
                        item,
                        &["type", "id", "summary", "encrypted_content", "status"],
                    ) {
                        b.issue("unknown_content");
                    }
                    if let Some(summary) = item["summary"].as_array() {
                        for part in summary {
                            if part["type"] == "summary_text" && part["text"].is_string() {
                                if extra_fields(part, &["type", "text"]) {
                                    b.issue("unknown_content");
                                }
                                b.text(
                                    ContextKind::Chat,
                                    "推理摘要",
                                    "reasoning",
                                    part["text"].as_str().unwrap(),
                                    &reference,
                                );
                            } else {
                                b.unknown(ContextKind::Chat, part, &reference);
                            }
                        }
                    } else if !item["summary"].is_null() {
                        b.unknown(ContextKind::Chat, &item["summary"], &reference);
                    }
                    if let Some(encrypted) = item
                        .get("encrypted_content")
                        .filter(|v| !v.is_null() && v.as_str() != Some(""))
                    {
                        b.opaque(ContextKind::Chat, "不透明推理", encrypted, &reference);
                    }
                }
                "compaction" | "compaction_summary" => {
                    b.opaque(ContextKind::Chat, "压缩上下文", item, &reference)
                }
                _ => b.unknown(ContextKind::Chat, item, &reference),
            }
        }
    } else if !input.is_null() {
        b.unknown(ContextKind::Chat, input, "");
    }
    b.finish()
}

fn read_files(name: &str, raw: &str, kind: tool::ToolKind) -> Option<Vec<String>> {
    if kind == tool::ToolKind::Custom {
        return (name == "exec")
            .then(|| code_mode_command(raw).and_then(|cmd| shell_files(&cmd)))
            .flatten();
    }
    let args: Value = serde_json::from_str(raw).ok()?;
    if matches!(
        name,
        "Read"
            | "read"
            | "read_file"
            | "ReadFile"
            | "view"
            | "View"
            | "NotebookRead"
            | "readFile"
            | "open_file"
            | "cat"
    ) {
        for key in [
            "file_path",
            "path",
            "filePath",
            "target_file",
            "notebook_path",
            "file",
            "filename",
            "uri",
        ] {
            if let Some(path) = args[key].as_str().filter(|s| !s.is_empty()) {
                return Some(vec![path.into()]);
            }
        }
    }
    if !matches!(name, "exec_command" | "shell" | "bash" | "shell_command") {
        return None;
    }
    let cmd = args["cmd"].as_str().or_else(|| args["command"].as_str())?;
    shell_files(cmd)
}

// One fixed CodeMode shape only: no variable evaluation, interpolation, nested
// calls, property getters, or arbitrary JavaScript interpretation.
fn code_mode_command(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches(';').trim();
    let args = raw
        .strip_prefix("text(await tools.exec_command(")?
        .strip_suffix("))")?
        .trim();
    let args = args.strip_prefix('{')?.strip_suffix('}')?.trim();
    let literal = args
        .strip_prefix("cmd:")
        .or_else(|| args.strip_prefix("\"cmd\":"))?
        .trim();
    serde_json::from_str::<String>(literal).ok()
}

fn shell_files(command: &str) -> Option<Vec<String>> {
    if command.contains([
        '|', ';', '&', '>', '<', '`', '$', '\n', '\r', '*', '?', '{', '}', '(', ')',
    ]) {
        return None;
    }
    // Small lexical reader supports quoted paths, never shell expansions.
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for ch in command.chars() {
        if ch == '\\' {
            return None;
        }
        if let Some(end) = quote {
            if ch == end {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    let cmd = words.first()?.as_str();
    if !matches!(cmd, "cat" | "head" | "tail" | "nl" | "bat" | "sed") {
        return None;
    }
    let mut index = 1;
    if cmd == "sed" {
        if words.get(index)? != "-n" {
            return None;
        }
        index += 1;
        let expression = words.get(index)?.strip_suffix('p')?;
        if expression.is_empty()
            || !expression
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == ',')
        {
            return None;
        }
        index += 1;
    }
    let mut files = Vec::new();
    let mut flags = true;
    while let Some(word) = words.get(index) {
        index += 1;
        if flags && word == "--" {
            flags = false;
            continue;
        }
        if flags && word.starts_with('-') {
            match (cmd, word.as_str()) {
                ("cat", "-n" | "-b" | "-s" | "-v" | "-A")
                | ("nl", "-ba" | "-bt")
                | ("bat", "--plain" | "-p" | "-n") => {}
                ("head" | "tail", "-n" | "-c") => {
                    let count = words.get(index)?;
                    if count.parse::<u64>().is_err() {
                        return None;
                    }
                    index += 1;
                }
                _ => return None,
            }
        } else {
            if word.is_empty() || word == "-" {
                return None;
            }
            files.push(word.clone());
        }
    }
    (!files.is_empty()).then_some(files)
}
