//! Replay the public, sanitized reading contract, never native input or raw wire data.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Document {
    pub request_id: Uuid,
    pub capture_seq: u64,
    pub document: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub record_seq: u64,
    #[serde(default)]
    pub earlier_before: Option<u64>,
    pub snapshot: Value,
    pub documents: Vec<Document>,
    pub details_partial: bool,
}
impl Checkpoint {
    pub fn sequence(&self) -> u64 {
        self.snapshot["viewSeq"].as_u64().unwrap_or(0)
    }
    pub fn validate(&self, epoch: Uuid) -> bool {
        self.snapshot["runEpoch"].as_str() == Some(epoch.to_string().as_str())
            && self.snapshot["schemaVersion"] == 2
            && self.snapshot["viewSeq"].as_u64().is_some()
            && [
                "items",
                "requests",
                "responses",
                "diagnostics",
                "toolContexts",
                "nativeCommands",
                "nativeFileChanges",
            ]
            .iter()
            .all(|key| self.snapshot[key].is_array())
    }
    pub fn document(&mut self, document: Document) {
        if self
            .documents
            .iter()
            .any(|d| d.request_id == document.request_id && d.document == document.document)
        {
            return;
        }
        self.documents.push(document);
        // Retain a bounded history view. Journals retain the source records;
        // this preview never represents evicted context as complete.
        let mut bytes: usize = self
            .documents
            .iter()
            .map(|d| d.document.to_string().len())
            .sum();
        while bytes > 16 * 1024 * 1024 || self.documents.len() > 512 {
            bytes -= self.documents.remove(0).document.to_string().len();
            self.details_partial = true;
        }
    }
    pub fn event(&mut self, event: &Value) -> Result<(), &'static str> {
        if event["runEpoch"] != self.snapshot["runEpoch"] {
            return Err("wrong_epoch");
        }
        let seq = event["viewSeq"].as_u64().ok_or("invalid_sequence")?;
        if seq != self.sequence() + 1 {
            return Err("sequence_gap");
        }
        match event["kind"].as_str() {
            Some("item.replace") => {
                let item = &event["item"];
                let key = item["itemKey"].as_str().ok_or("invalid_item")?;
                let revision = item["revision"].as_u64().ok_or("invalid_revision")?;
                let items = array(&mut self.snapshot, "items")?;
                if let Some(previous) = items.iter_mut().find(|v| v["itemKey"] == key) {
                    if previous["kind"] != item["kind"]
                        || previous["author"]["role"] != item["author"]["role"]
                    {
                        return Err("item_type_conflict");
                    }
                    if revision > previous["revision"].as_u64().ok_or("invalid_revision")? {
                        *previous = item.clone();
                    }
                } else {
                    items.push(item.clone());
                }
                if item["completeness"] != "observed" {
                    self.snapshot["capture"] = json!("partial");
                }
            }
            Some("item.patch") => {
                let items = array(&mut self.snapshot, "items")?;
                let item = items
                    .iter_mut()
                    .find(|v| v["itemKey"] == event["itemKey"])
                    .ok_or("missing_item")?;
                let base = event["baseRevision"].as_u64().ok_or("invalid_revision")?;
                if item["revision"] != base || event["revision"].as_u64() != base.checked_add(1) {
                    return Err("revision_gap");
                }
                let append = event["append"].as_str().ok_or("invalid_patch")?;
                let field = match event["field"].as_str() {
                    Some("text")
                        if item["kind"] == "message" && item["author"]["role"] == "assistant" =>
                    {
                        let part = array(item, "content")?
                            .iter_mut()
                            .find(|p| p["contentKey"] == event["contentKey"])
                            .ok_or("missing_content")?;
                        &mut part["text"]
                    }
                    Some("arguments") if item["kind"] == "tool_call" => &mut item["arguments"],
                    _ => return Err("invalid_patch_type"),
                };
                let mut text = field.as_str().ok_or("invalid_text")?.to_owned();
                if text.len() + append.len() > 2 * 1024 * 1024 {
                    return Err("item_capacity");
                }
                text.push_str(append);
                *field = json!(text);
                item["revision"] = event["revision"].clone();
                item["truncated"] = event["truncated"].clone();
                if event["truncated"] == true {
                    item["completeness"] = json!("partial");
                    self.snapshot["capture"] = json!("partial");
                }
            }
            Some("request.metadata") => upsert(
                &mut self.snapshot,
                "requests",
                &event["request"],
                &["requestId", "clientRequestIndex"],
            )?,
            Some("request.state") => upsert(
                &mut self.snapshot,
                "responses",
                &event["response"],
                &["requestId", "responseId"],
            )?,
            Some("tool.context") => upsert(
                &mut self.snapshot,
                "toolContexts",
                &event["context"],
                &["requestId", "clientRequestIndex"],
            )?,
            Some("native.command") => upsert(
                &mut self.snapshot,
                "nativeCommands",
                &event["command"],
                &["source"],
            )?,
            Some("native.file_change") => upsert(
                &mut self.snapshot,
                "nativeFileChanges",
                &event["change"],
                &["source"],
            )?,
            Some("user.capture") => {
                self.snapshot["userCapture"] = event["userCapture"].clone();
                if event["userCapture"]["diagnostics"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                {
                    self.snapshot["capture"] = json!("partial");
                }
            }
            Some("capture.gap") => {
                upsert(
                    &mut self.snapshot,
                    "diagnostics",
                    &event["diagnostic"],
                    &["requestId", "code"],
                )?;
                self.snapshot["capture"] = json!("partial");
            }
            _ => return Err("unknown_event"),
        }
        if let Some(summary) = event.get("usageSummary") {
            self.snapshot["usageSummary"] = summary.clone();
        }
        self.snapshot["viewSeq"] = json!(seq);
        Ok(())
    }
}
fn array<'a>(value: &'a mut Value, key: &str) -> Result<&'a mut Vec<Value>, &'static str> {
    value[key].as_array_mut().ok_or("invalid_snapshot")
}
fn upsert(
    snapshot: &mut Value,
    field: &str,
    value: &Value,
    keys: &[&str],
) -> Result<(), &'static str> {
    if !value.is_object() {
        return Err("invalid_event");
    }
    let items = array(snapshot, field)?;
    if let Some(previous) = items
        .iter_mut()
        .find(|v| keys.iter().all(|key| v[*key] == value[*key]))
    {
        *previous = value.clone();
    } else {
        items.push(value.clone());
    }
    Ok(())
}
