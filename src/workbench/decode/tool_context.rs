//! Closed request tool definitions and result evidence. No arbitrary JSON fields
//! or input role inference are carried into the live view.
use super::tool::{self, PREVIEW_BYTES, ToolIdentity, ToolKind};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Command,
    Code,
    Patch,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub namespace: Option<String>,
    pub tool_kind: ToolKind,
    pub category: ToolCategory,
    pub source: &'static str,
    pub input_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFacts {
    pub exit_code: Option<i32>,
    pub duration_ms: f64,
    pub running: bool,
    pub process_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvidence {
    pub input_index: u32,
    pub tool_kind: ToolKind,
    pub call_id: Option<String>,
    pub output: SafeText,
    pub truncated: bool,
    pub omitted: bool,
    #[serde(skip)]
    pub fingerprint: Option<blake3::Hash>,
    #[serde(skip)]
    pub companion: Option<ToolIdentity>,
    #[serde(skip)]
    pub command_facts: Option<CommandFacts>,
    #[serde(skip)]
    pub command_output: Option<SafeText>,
    #[serde(skip)]
    pub output_fingerprint: blake3::Hash,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolContext {
    pub client_request_index: Option<u64>,
    pub definitions: Vec<ToolDefinition>,
    pub outputs: Vec<OutputEvidence>,
    pub partial: bool,
}

fn definitions(
    values: &Value,
    namespace: Option<String>,
    source: &'static str,
    input_index: Option<u32>,
    policy: &Arc<RedactionPolicy>,
    context: &mut ToolContext,
) {
    let Some(values) = values.as_array() else {
        return;
    };
    for value in values {
        if context.definitions.len() >= 256 {
            context.partial = true;
            break;
        }
        if value["type"] == "namespace" {
            if namespace.is_some() {
                context.partial = true;
                continue;
            }
            if let Some(name) = tool::identifier(&value["name"], policy) {
                definitions(
                    &value["tools"],
                    Some(name),
                    source,
                    input_index,
                    policy,
                    context,
                );
            } else {
                context.partial = true;
            }
            continue;
        }
        let tool_kind = match value["type"].as_str() {
            Some("function") => ToolKind::Function,
            Some("custom") => ToolKind::Custom,
            _ => {
                context.partial = true;
                continue;
            }
        };
        let Some(name) = tool::identifier(&value["name"], policy) else {
            context.partial = true;
            continue;
        };
        let native_namespace = namespace.as_deref().is_none_or(|name| name == "functions");
        let category = match (tool_kind, name.as_str(), native_namespace) {
            (ToolKind::Function, "exec_command", true)
                if value["parameters"]["type"] == "object"
                    && value["parameters"]["properties"]["cmd"]["type"] == "string" =>
            {
                ToolCategory::Command
            }
            (ToolKind::Custom, "exec", true) if value["format"]["type"] == "grammar" => {
                ToolCategory::Code
            }
            (ToolKind::Custom, "apply_patch", true) if value["format"]["type"] == "grammar" => {
                ToolCategory::Patch
            }
            _ => ToolCategory::Other,
        };
        context.definitions.push(ToolDefinition {
            name,
            namespace: namespace.clone(),
            tool_kind,
            category,
            source,
            input_index,
        });
    }
}

fn output_text(value: &Value) -> (String, bool) {
    if let Some(text) = value.as_str() {
        return (text.to_owned(), false);
    }
    let Some(parts) = value.as_array() else {
        return (String::new(), true);
    };
    let mut text = String::new();
    let mut omitted = false;
    for part in parts {
        if part["type"] == "input_text"
            && let Some(value) = part["text"].as_str()
        {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(value);
        } else {
            omitted = true;
        }
    }
    (text, omitted)
}

// This is the installed native exec_command envelope, anchored at the start.
// Never search arbitrary code/MCP prose for exit-code-looking text.
pub(crate) fn command_facts(raw: &str) -> Option<(CommandFacts, &str)> {
    let (header, output) = raw
        .split_once("\nOutput:\n")
        .or_else(|| raw.strip_suffix("\nOutput:").map(|header| (header, "")))?;
    if header.len() > 512 {
        return None;
    }
    let mut lines = header.lines().peekable();
    if lines.peek()?.starts_with("Chunk ID: ") {
        let chunk = lines.next()?.strip_prefix("Chunk ID: ")?;
        if chunk.is_empty()
            || !chunk
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return None;
        }
    }
    let duration: f64 = lines
        .next()?
        .strip_prefix("Wall time: ")?
        .strip_suffix(" seconds")?
        .parse()
        .ok()?;
    if !duration.is_finite() || duration < 0.0 || !(duration * 1000.0).is_finite() {
        return None;
    }
    let state = lines.next()?;
    let (exit_code, process_id) =
        if let Some(code) = state.strip_prefix("Process exited with code ") {
            (Some(code.parse::<i32>().ok()?), None)
        } else if let Some(id) = state.strip_prefix("Process running with session ID ") {
            if id.parse::<u32>().ok()? == 0 {
                return None;
            }
            (None, Some(id.to_owned()))
        } else {
            return None;
        };
    if let Some(line) = lines.next() {
        line.strip_prefix("Original token count: ")?
            .parse::<u64>()
            .ok()?;
    }
    if lines.next().is_some() {
        return None;
    }
    Some((
        CommandFacts {
            exit_code,
            duration_ms: duration * 1000.0,
            running: process_id.is_some(),
            process_id,
        },
        output,
    ))
}

pub fn extract(
    body: &Value,
    client_request_index: Option<u64>,
    policy: &Arc<RedactionPolicy>,
) -> ToolContext {
    let mut context = ToolContext {
        client_request_index,
        definitions: Vec::new(),
        outputs: Vec::new(),
        partial: false,
    };
    definitions(&body["tools"], None, "tools", None, policy, &mut context);
    let Some(input) = body["input"].as_array() else {
        return context;
    };
    let mut calls: HashMap<String, Vec<(ToolIdentity, blake3::Hash)>> = HashMap::new();
    for (index, item) in input.iter().enumerate().take(4096) {
        if item["type"] == "additional_tools" && item["role"] == "developer" {
            definitions(
                &item["tools"],
                None,
                "additional_tools",
                Some(index as u32),
                policy,
                &mut context,
            );
        }
        if let Some(kind) = tool::kind(item) {
            let identity = tool::identity(item, kind, policy);
            if let (Some(call), Some(raw)) = (&identity.call_id, tool::arguments(item, kind))
                && let Some(hash) = tool::fingerprint(&identity, raw)
            {
                calls
                    .entry(call.clone())
                    .or_default()
                    .push((identity, hash));
            }
        }
    }
    if input.len() > 4096 {
        context.partial = true;
    }
    for (index, item) in input.iter().enumerate().take(4096) {
        let tool_kind = match item["type"].as_str() {
            Some("function_call_output") => ToolKind::Function,
            Some("custom_tool_call_output") => ToolKind::Custom,
            _ => continue,
        };
        if context.outputs.len() >= 128 {
            context.partial = true;
            break;
        }
        let call_id = tool::identifier(&item["call_id"], policy);
        let candidate = call_id
            .as_ref()
            .and_then(|id| calls.get(id))
            .filter(|values| values.len() == 1)
            .and_then(|values| values.first())
            .filter(|(identity, _)| identity.tool_kind == tool_kind);
        let (raw, omitted) = output_text(&item["output"]);
        let safe = policy.scrub_tool(&raw);
        let parsed = (!omitted).then(|| command_facts(&raw)).flatten();
        context.outputs.push(OutputEvidence {
            input_index: index as u32,
            tool_kind,
            call_id,
            truncated: safe.as_str().len() > PREVIEW_BYTES
                || super::output::has_truncation_marker(&raw),
            output: safe.bounded(PREVIEW_BYTES),
            omitted,
            fingerprint: candidate.map(|(_, hash)| *hash),
            companion: candidate.map(|(identity, _)| identity.clone()),
            command_facts: parsed.as_ref().map(|(facts, _)| facts.clone()),
            command_output: parsed.map(|(_, text)| policy.scrub_tool(text).bounded(PREVIEW_BYTES)),
            output_fingerprint: blake3::hash(
                &serde_json::to_vec(&item["output"]).unwrap_or_default(),
            ),
        });
    }
    context
}
