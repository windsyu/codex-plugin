//! Bounded, sanitized request/response documents for on-demand reading.
//! Headers, opaque metadata and binary/encrypted content have no output path.
use super::*;
use serde_json::{Map, json};

const PREVIEW_BYTES: usize = 24 * 1024;
const DOCUMENT_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DetailSource {
    Request { client_request_index: Option<u64> },
    Response { response_id: Option<String> },
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailEntry {
    pub section: &'static str,
    pub position: Option<u32>,
    pub json_pointer: String,
    pub children_separated: bool,
    pub preview: String,
    pub truncated: bool,
    pub omitted: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailDocument {
    pub source: DetailSource,
    pub entries: Vec<DetailEntry>,
    pub truncated: bool,
    pub omitted: bool,
}
impl DetailDocument {
    pub fn bytes(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.preview.len() * 2 + 512)
            .sum::<usize>()
            + 256
    }
}

struct Scrubber<'a> {
    policy: &'a Arc<RedactionPolicy>,
    nodes: usize,
    truncated: bool,
    omitted: bool,
}
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Content,
    Schema,
    SensitiveSchema,
    Properties,
}
const SCHEMA_KEYS: &[&str] = &[
    "type",
    "title",
    "description",
    "properties",
    "required",
    "items",
    "prefixItems",
    "additionalProperties",
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    "$ref",
    "$defs",
    "definitions",
    "enum",
    "const",
    "format",
    "pattern",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "uniqueItems",
    "multipleOf",
    "nullable",
];
fn private_field(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    normalized.starts_with("encrypted")
        || matches!(
            normalized.as_str(),
            "authorization"
                | "cookie"
                | "setcookie"
                | "password"
                | "token"
                | "apikey"
                | "accesstoken"
                | "refreshtoken"
                | "clientsecret"
                | "privatekey"
                | "env"
                | "environment"
                | "imageurl"
                | "imagebase64"
                | "filedata"
                | "base64"
                | "audio"
                | "headers"
                | "clientmetadata"
                | "metadata"
        )
}
impl Scrubber<'_> {
    fn value(&mut self, value: &Value, depth: usize, shape: Shape) -> Value {
        if self.nodes == 0 || depth > 12 {
            self.truncated = true;
            return json!("[超过阅读预算]");
        }
        self.nodes -= 1;
        match value {
            Value::String(raw) => {
                let safe = self.policy.scrub_tool(raw);
                self.omitted |= safe.as_str() != raw;
                self.truncated |= safe.as_str().len() > PREVIEW_BYTES;
                json!(safe.bounded(PREVIEW_BYTES).as_str())
            }
            Value::Array(values) => {
                self.truncated |= values.len() > 256;
                Value::Array(
                    values
                        .iter()
                        .take(256)
                        .map(|value| self.value(value, depth + 1, shape))
                        .collect(),
                )
            }
            Value::Object(object) => {
                if matches!(
                    object.get("type").and_then(Value::as_str),
                    Some(
                        "input_image"
                            | "output_image"
                            | "input_file"
                            | "image_generation_call"
                            | "input_audio"
                            | "output_audio"
                            | "image"
                            | "audio"
                            | "encrypted_content"
                    )
                ) {
                    self.omitted = true;
                    return json!({"type":"omitted","reason":"media_or_encrypted_content"});
                }
                self.truncated |= object.len() > 256;
                let mut safe = Map::new();
                for (key, value) in object.iter().take(256) {
                    let name = self.policy.scrub(key);
                    if name.as_str() != key || key.len() > 128 {
                        self.omitted = true;
                        continue;
                    }
                    // Properties are schema declarations, not secret values.
                    // Defaults/examples are excluded even inside a schema.
                    if (shape != Shape::Properties && private_field(key))
                        || (shape == Shape::Schema && !SCHEMA_KEYS.contains(&key.as_str()))
                        || (shape == Shape::SensitiveSchema
                            && !["type", "description", "title", "format", "nullable"]
                                .contains(&key.as_str()))
                        || (shape == Shape::Properties && !value.is_object() && !value.is_boolean())
                    {
                        self.omitted = true;
                        safe.insert(key.clone(), json!("[按策略省略]"));
                        continue;
                    }
                    let child_shape = if shape == Shape::Properties {
                        if private_field(key) {
                            Shape::SensitiveSchema
                        } else {
                            Shape::Schema
                        }
                    } else if matches!(shape, Shape::Schema | Shape::SensitiveSchema)
                        && matches!(key.as_str(), "properties" | "$defs" | "definitions")
                    {
                        Shape::Properties
                    } else if shape == Shape::Schema
                        || (key == "parameters"
                            && object.get("type").is_some_and(|value| value == "function"))
                        || (key == "schema"
                            && object
                                .get("type")
                                .is_some_and(|value| value == "json_schema"))
                    {
                        Shape::Schema
                    } else {
                        Shape::Content
                    };
                    safe.insert(key.clone(), self.value(value, depth + 1, child_shape));
                }
                Value::Object(safe)
            }
            _ => value.clone(),
        }
    }
}

struct DocumentBuilder<'a> {
    document: DetailDocument,
    scrubber: Scrubber<'a>,
    bytes: usize,
}
impl<'a> DocumentBuilder<'a> {
    fn new(source: DetailSource, policy: &'a Arc<RedactionPolicy>) -> Self {
        Self {
            document: DetailDocument {
                source,
                entries: Vec::new(),
                truncated: false,
                omitted: false,
            },
            scrubber: Scrubber {
                policy,
                nodes: 8192,
                truncated: false,
                omitted: false,
            },
            bytes: 0,
        }
    }
    fn entry(&mut self, section: &'static str, position: Option<u32>, value: &Value) {
        let pointer = if matches!(section, "settings" | "response") {
            String::new()
        } else {
            format!(
                "/{section}{}",
                position
                    .map(|index| format!("/{index}"))
                    .unwrap_or_default()
            )
        };
        self.entry_at(section, position, pointer, value, false);
    }
    fn entry_at(
        &mut self,
        section: &'static str,
        position: Option<u32>,
        json_pointer: String,
        value: &Value,
        children_separated: bool,
    ) {
        if self.document.entries.len() >= 1024 || self.bytes >= DOCUMENT_BYTES {
            self.document.truncated = true;
            return;
        }
        self.scrubber.truncated = false;
        self.scrubber.omitted = false;
        let safe = self.scrubber.value(value, 0, Shape::Content);
        let json = serde_json::to_string_pretty(&safe).expect("safe document JSON");
        let budget = PREVIEW_BYTES.min((DOCUMENT_BYTES - self.bytes).saturating_sub(512) / 2);
        let preview = crate::workbench::live::prefix(&json, budget).to_owned();
        let truncated = self.scrubber.truncated || preview.len() < json.len();
        let omitted = self.scrubber.omitted;
        self.bytes += preview.len() * 2 + 512;
        self.document.truncated |= truncated;
        self.document.omitted |= omitted;
        self.document.entries.push(DetailEntry {
            section,
            position,
            json_pointer,
            children_separated,
            preview,
            truncated,
            omitted,
        });
    }
    fn list(&mut self, section: &'static str, value: &Value) {
        if let Some(values) = value.as_array() {
            self.document.truncated |= values.len() > 1024;
            for (index, value) in values.iter().enumerate().take(1024) {
                self.entry(section, Some(index as u32), value);
            }
        } else if !value.is_null() {
            self.entry(section, None, value);
        }
    }
    fn tools(&mut self, value: &Value, pointer: &str, depth: usize) {
        if depth > 8 {
            self.document.truncated = true;
            return;
        }
        let Some(tools) = value.as_array() else {
            if !value.is_null() {
                self.entry_at("tools", None, pointer.into(), value, false);
            }
            return;
        };
        self.document.truncated |= tools.len() > 1024;
        for (index, tool) in tools.iter().enumerate().take(1024) {
            let path = format!("{pointer}/{index}");
            let grouped = tool["type"] == "namespace" && tool["tools"].is_array();
            if grouped {
                let mut metadata = tool.as_object().unwrap().clone();
                metadata.remove("tools");
                self.entry_at(
                    "tools",
                    Some(index as u32),
                    path.clone(),
                    &Value::Object(metadata),
                    true,
                );
                self.tools(&tool["tools"], &format!("{path}/tools"), depth + 1);
            } else {
                self.entry_at("tools", Some(index as u32), path, tool, false);
            }
        }
    }
    fn input(&mut self, value: &Value) {
        let Some(items) = value.as_array() else {
            self.list("input", value);
            return;
        };
        self.document.truncated |= items.len() > 1024;
        for (index, item) in items.iter().enumerate().take(1024) {
            let grouped = item["type"] == "additional_tools"
                && item["role"] == "developer"
                && item["tools"].is_array();
            if grouped {
                let mut metadata = item.as_object().unwrap().clone();
                metadata.remove("tools");
                self.entry_at(
                    "input",
                    Some(index as u32),
                    format!("/input/{index}"),
                    &Value::Object(metadata),
                    true,
                );
                self.tools(&item["tools"], &format!("/input/{index}/tools"), 0);
            } else {
                self.entry("input", Some(index as u32), item);
            }
        }
    }
}

pub fn request(body: &Value, index: Option<u64>, policy: &Arc<RedactionPolicy>) -> DetailDocument {
    let mut builder = DocumentBuilder::new(
        DetailSource::Request {
            client_request_index: index,
        },
        policy,
    );
    let fields = [
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
    ];
    let settings: Map<_, _> = fields
        .into_iter()
        .filter_map(|key| body.get(key).map(|value| (key.to_owned(), value.clone())))
        .collect();
    builder.entry("settings", None, &Value::Object(settings));
    if let Some(instructions) = body.get("instructions") {
        builder.entry("instructions", None, instructions);
    }
    builder.input(&body["input"]);
    builder.tools(&body["tools"], "/tools", 0);
    // Raw metadata, credentials and unknown extensions are outside this view.
    builder.document.omitted |= body.as_object().is_none_or(|object| {
        object.keys().any(|key| {
            !fields.contains(&key.as_str())
                && !["instructions", "input", "tools", "type"].contains(&key.as_str())
        })
    });
    builder.document
}

pub fn response(body: &Value, policy: &Arc<RedactionPolicy>) -> DetailDocument {
    let mut builder = DocumentBuilder::new(
        DetailSource::Response {
            response_id: tool::identifier(&body["id"], policy),
        },
        policy,
    );
    let fields = [
        "id",
        "model",
        "status",
        "usage",
        "service_tier",
        "error",
        "incomplete_details",
    ];
    let metadata: Map<_, _> = fields
        .into_iter()
        .filter_map(|key| body.get(key).map(|value| (key.to_owned(), value.clone())))
        .collect();
    builder.entry("response", None, &Value::Object(metadata));
    if body.get("output").is_none() {
        builder.document.omitted = true;
    }
    builder.list("output", &body["output"]);
    builder.document.omitted |= body.as_object().is_none_or(|object| {
        object
            .keys()
            .any(|key| !fields.contains(&key.as_str()) && key != "output")
    });
    builder.document
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub invalid: bool,
}
pub fn usage(value: &Value) -> Option<ResponseUsage> {
    if value.is_null() {
        return None;
    }
    let mut invalid = !value.is_object()
        || ["input_tokens_details", "output_tokens_details"]
            .iter()
            .any(|key| {
                value
                    .get(*key)
                    .is_some_and(|value| !value.is_null() && !value.is_object())
            });
    let mut number = |value: &Value| {
        let parsed = value.as_u64().filter(|n| *n <= 9_007_199_254_740_991);
        invalid |= !value.is_null() && parsed.is_none();
        parsed
    };
    let mut result = ResponseUsage {
        input_tokens: number(&value["input_tokens"]),
        output_tokens: number(&value["output_tokens"]),
        total_tokens: number(&value["total_tokens"]),
        cached_input_tokens: number(&value["input_tokens_details"]["cached_tokens"]),
        cache_write_tokens: number(&value["input_tokens_details"]["cache_write_tokens"]),
        reasoning_tokens: number(&value["output_tokens_details"]["reasoning_tokens"]),
        invalid: false,
    };
    result.invalid = invalid
        || result
            .cached_input_tokens
            .zip(result.input_tokens)
            .is_some_and(|(cached, input)| cached > input)
        || result
            .reasoning_tokens
            .zip(result.output_tokens)
            .is_some_and(|(reasoning, output)| reasoning > output)
        || result
            .input_tokens
            .zip(result.output_tokens)
            .zip(result.total_tokens)
            .is_some_and(|((input, output), total)| input + output != total);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_context_preserves_schema_and_roles_without_credentials_or_binary_payloads() {
        let policy = RedactionPolicy::new(vec!["synthetic-credential".into()]).unwrap();
        let doc = request(
            &json!({"model":"gpt-fixture","instructions":"system synthetic-credential","input":[{"role":"developer","content":"literal <svg>"},{"role":"user","content":[{"type":"input_image","image_url":"PRIVATE_BASE64"}]},{"type":"reasoning","summary":[{"type":"summary_text","text":"visible summary"}],"encrypted_content":"PRIVATE_REASONING"}],"tools":[{"type":"function","name":"login","parameters":{"type":"object","properties":{"password":{"type":"string","description":"declared field","default":"PRIVATE_DEFAULT"}}}}],"headers":{"Authorization":"PRIVATE_HEADER"},"client_metadata":{"workspaces":{"PRIVATE_PATH":"PRIVATE_VALUE"}}}),
            None,
            &policy,
        );
        let text = serde_json::to_string(&doc).unwrap();
        for hidden in [
            "synthetic-credential",
            "PRIVATE_BASE64",
            "PRIVATE_REASONING",
            "PRIVATE_DEFAULT",
            "PRIVATE_HEADER",
            "PRIVATE_PATH",
            "PRIVATE_VALUE",
        ] {
            assert!(!text.contains(hidden));
        }
        assert!(
            text.contains("declared field")
                && text.contains("password")
                && text.contains("visible summary")
                && text.contains("developer")
        );
        assert!(doc.omitted);
        let adversarial = request(
            &json!({"input":[{"type":"message","properties":{"password":"PRIVATE_FAKE_SCHEMA"}},{"type":"function","parameters":{"properties":{"token":"PRIVATE_NON_SCHEMA","password":{"value":"PRIVATE_SCHEMA_EXTENSION","type":"string","const":"PRIVATE_CONST","enum":["PRIVATE_ENUM"]}}}},{"type":"input_file","file_data":"PRIVATE_FILE"},{"type":"image_generation_call","result":"PRIVATE_GENERATED_IMAGE"}]}),
            None,
            &policy,
        );
        assert!(
            !serde_json::to_string(&adversarial)
                .unwrap()
                .contains("PRIVATE_")
        );
    }
    #[test]
    fn documents_are_bounded_and_ws_sources_remain_separate_from_responses() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        let doc = request(
            &json!({"input":vec![json!({"role":"user","content":"中".repeat(20000)});100]}),
            Some(2),
            &policy,
        );
        assert!(doc.truncated && doc.bytes() < DOCUMENT_BYTES + 1024);
        assert!(
            doc.entries
                .iter()
                .all(|entry| entry.preview.len() <= PREVIEW_BYTES)
        );
        assert_eq!(
            doc.source,
            DetailSource::Request {
                client_request_index: Some(2)
            }
        );
        let response = response(
            &json!({"id":"response-one","output":[{"type":"message","content":[{"type":"output_text","text":"safe"}]}]}),
            &policy,
        );
        assert_eq!(
            response.source,
            DetailSource::Response {
                response_id: Some("response-one".into())
            }
        );
    }
    #[test]
    fn large_developer_and_namespace_tool_sets_keep_individual_schemas_readable() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        let tools: Vec<_> = (0..20).map(|index| json!({"type":"function","name":format!("read_{index}"),"description":"explanation ".repeat(300),"parameters":{"type":"object","properties":{"path":{"type":"string"}}}})).collect();
        let doc = request(
            &json!({"input":[{"type":"additional_tools","role":"developer","tools":[{"type":"namespace","name":"functions","tools":tools}]}]}),
            None,
            &policy,
        );
        let schemas: Vec<Value> = doc
            .entries
            .iter()
            .filter_map(|entry| serde_json::from_str::<Value>(&entry.preview).ok())
            .filter(|value| value["type"] == "function")
            .collect();
        assert_eq!(
            schemas.len(),
            20,
            "a tool set must be paged by definition rather than cut as one oversized message"
        );
        assert_eq!(
            schemas[19]["parameters"]["properties"]["path"]["type"],
            "string"
        );
    }
    #[test]
    fn usage_keeps_missing_fields_unknown_and_rejects_invalid_or_inconsistent_numbers() {
        assert!(usage(&Value::Null).is_none());
        let count = usage(&json!({"input_tokens":20,"output_tokens":4,"total_tokens":24,"input_tokens_details":{"cached_tokens":5}})).unwrap();
        assert!(!count.invalid && count.reasoning_tokens.is_none());
        for value in [
            json!({"input_tokens":-1}),
            json!({"output_tokens":"7"}),
            json!({"total_tokens":9007199254740992u64}),
            json!({"input_tokens":2,"output_tokens":3,"total_tokens":10}),
            json!({"input_tokens":2,"input_tokens_details":{"cached_tokens":3}}),
        ] {
            assert!(usage(&value).unwrap().invalid);
        }
    }
}
