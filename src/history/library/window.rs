//! On-demand, revision-pinned source reading on the history query worker.
//! Locators never leave the derived database; clients receive signed cursors.
use super::*;
use crate::history::legacy_reader::{Collection, LegacyReader, Query as LegacyQuery, ReadBudget};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

pub(super) struct Window {
    pub records: Vec<(Value, Value)>,
    pub next: Option<Value>,
    pub reasons: Vec<String>,
}
fn seal(shared: &Shared, entry: &Entry, state: &Value, details: bool) -> String {
    let body = json!([entry.entry_id, entry.source_revision, details, state]).to_string();
    URL_SAFE_NO_PAD.encode(
        json!([
            body,
            blake3::keyed_hash(&shared.key, body.as_bytes())
                .to_hex()
                .to_string()
        ])
        .to_string(),
    )
}
fn unseal(shared: &Shared, entry: &Entry, token: &str, details: bool) -> Result<Value> {
    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| "invalid_cursor")?;
    let value: Value = serde_json::from_slice(&raw).map_err(|_| "invalid_cursor")?;
    let body = value[0].as_str().ok_or("invalid_cursor")?;
    if value[1].as_str()
        != Some(
            blake3::keyed_hash(&shared.key, body.as_bytes())
                .to_hex()
                .as_str(),
        )
    {
        return Err("invalid_cursor");
    }
    let payload: Value = serde_json::from_str(body).map_err(|_| "invalid_cursor")?;
    if payload[0] != entry.entry_id || payload[1] != entry.source_revision || payload[2] != details
    {
        return Err("source_revision_changed");
    }
    Ok(payload[3].clone())
}
pub(super) fn read(
    shared: &Shared,
    sources: &[Source],
    query: Query,
    id: String,
    details: bool,
) -> Result<Value> {
    if query.record.is_some_and(|n| n > 10_000_000)
        || query.project_id.is_some()
        || query.kind.is_some()
        || query.source_id.is_some()
        || query.group.is_some()
        || query.q.is_some()
    {
        return Err("invalid_query");
    }
    let db = read_db(shared)?.ok_or("entry_unavailable")?;
    db.execute_batch("BEGIN DEFERRED")
        .map_err(|_| "cache_busy")?;
    let identities =
        serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>()).unwrap();
    let found:Option<(String,String,String)>=db.query_row("SELECT e.metadata,l.locator,e.generation FROM catalog_entries e JOIN catalog_sources s ON s.generation=e.generation JOIN catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.id=?1 AND s.identity IN(SELECT value FROM json_each(?2))",params![id,identities],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|"cache_unavailable")?;
    let (metadata, locator, generation) = found.ok_or("entry_unavailable")?;
    let entry: Entry = serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?;
    let locator: Value = serde_json::from_str(&locator).map_err(|_| "cache_invalid")?;
    if query
        .source_revision
        .as_ref()
        .is_some_and(|s| s != &entry.source_revision)
    {
        return Err("source_revision_changed");
    }
    let source = sources
        .iter()
        .find(|s| s.id() == entry.source_id)
        .ok_or("source_revoked")?;
    let mut state = match &query.cursor {
        Some(c) => unseal(shared, &entry, c, details)?,
        None => json!({}),
    };
    if let Some(record) = query.record {
        if query.cursor.is_some() || details {
            return Err("invalid_query");
        }
        let raw: Option<String> = db
            .query_row(
                "SELECT body FROM catalog_records WHERE generation=?1 AND entry=?2 AND seq=?3",
                params![generation, id, record],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| "cache_unavailable")?;
        let raw: Value =
            serde_json::from_str(&raw.ok_or("entry_unavailable")?).map_err(|_| "cache_invalid")?;
        state = match source {
            Source::Native { .. } => json!({"offset":raw["recordId"].as_u64().unwrap_or(0)}),
            Source::Observer { .. } => json!({"skip":record}),
            Source::Workbench { .. } => json!({"target":record_identity(&raw)}),
        };
    }
    let generations: Vec<String> = db.prepare("SELECT generation FROM catalog_sources WHERE identity IN(SELECT value FROM json_each(?1))")
        .and_then(|mut statement| statement.query_map([&identities], |row| row.get(0))?.collect())
        .map_err(|_| "cache_unavailable")?;
    let mut entry_value = serde_json::to_value(&entry).unwrap();
    relate(
        &db,
        sources,
        &serde_json::to_string(&generations).unwrap(),
        &mut entry_value,
    )?;
    // No database transaction remains open while the selected source is read.
    drop(db);
    let limit = if details {
        1
    } else {
        query.limit.unwrap_or(16).min(32)
    };
    let window = match source {
        Source::Native { .. } => native::window(
            source,
            &locator,
            &entry,
            &state,
            limit,
            details,
            &shared.key,
        )?,
        Source::Observer { .. } => {
            observer(shared, source, &locator, &entry, &state, limit, details)?
        }
        Source::Workbench { data_directory, .. } => {
            let epoch = entry
                .run_id
                .as_deref()
                .and_then(|v| uuid::Uuid::parse_str(v).ok())
                .ok_or("locator_unavailable")?;
            let before = state["before"].as_u64();
            let (records, earlier, reasons) = crate::workbench::recording::library::window(
                data_directory,
                epoch,
                &entry.source_revision,
                before,
            )?;
            let target = state["target"].as_str();
            let target_index = target
                .map(|t| {
                    records
                        .iter()
                        .position(|r| record_identity(r) == t)
                        .ok_or("search_position_unavailable")
                })
                .transpose()?;
            let index = target_index
                .map(|i| i as u64)
                .or(state["index"].as_u64())
                .or(state["skip"].as_u64())
                .unwrap_or(0) as usize;
            let length = records.len();
            let records = records
                .into_iter()
                .enumerate()
                .skip(index)
                .take(limit)
                .map(|(i, r)| (r, json!({"before":before,"index":i})))
                .collect::<Vec<_>>();
            let next = if index + records.len() < length {
                Some(json!({"before":before,"index":index+records.len()}))
            } else {
                earlier.map(|b| json!({"before":b,"index":0}))
            };
            Window {
                records,
                next,
                reasons,
            }
        }
    };
    let mut records = vec![];
    let mut bytes = 0;
    let mut next = window.next;
    for (record, position) in window.records {
        let safe = sanitize(&record, &shared.key);
        let mut value = if details { safe } else { preview(&safe) };
        if value.to_string().len() > 800 * 1024 {
            value = json!({"kind":value["kind"],"detailsOmitted":true,"text":"此条详情超过读取限制，未返回完整内容。"});
        }
        let size = value.to_string().len();
        if bytes + size > 880 * 1024 {
            next = Some(position);
            break;
        }
        bytes += size;
        value["detailCursor"] = json!(seal(shared, &entry, &position, true));
        records.push(value);
    }
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    let mut reasons = entry
        .coverage
        .reasons
        .iter()
        .filter(|s| {
            !matches!(
                s.as_str(),
                "body_cache_limit"
                    | "cache_budget"
                    | "run_read_budget"
                    | "earlier_window_not_cached"
                    | "detail_limit"
                    | "search_excerpt_limit"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    reasons.extend(window.reasons);
    if next.is_some() {
        reasons.push("windowed_read".into());
    }
    Ok(
        json!({"revision":entry.source_revision,"entry":entry_value,"records":records,"nextCursor":next.map(|s|seal(shared,&entry,&s,details)),"coverage":{"state":if reasons.is_empty(){"complete_for_source"}else{"partial"},"reasons":reasons},"window":state["before"]}),
    )
}
fn observer(
    _shared: &Shared,
    source: &Source,
    locator: &Value,
    entry: &Entry,
    state: &Value,
    limit: usize,
    details: bool,
) -> Result<Window> {
    let Source::Observer {
        id,
        database,
        blob_directory,
        ..
    } = source
    else {
        unreachable!()
    };
    let budget = ReadBudget::default();
    let reader = LegacyReader::open(id, database, blob_directory.as_deref(), &budget)
        .map_err(|_| "source_unavailable")?
        .for_library(*blake3::hash(source.identity().as_bytes()).as_bytes());
    let scope = locator["thread"]
        .as_str()
        .ok_or("locator_unavailable")?
        .to_owned();
    let mut phase = state["phase"].as_str().unwrap_or("context").to_owned();
    let mut cursor = state["cursor"].as_str().map(String::from);
    let mut skip = state["skip"].as_u64().unwrap_or(0).saturating_sub(1);
    let mut records = vec![];
    let mut reasons = vec![];
    let mut next = None;
    for _ in 0..(limit + skip as usize).min(4096) {
        let position = json!({"phase":phase,"cursor":cursor});
        let page = reader
            .page(
                &LegacyQuery {
                    collection: if phase == "context" {
                        Collection::Context
                    } else {
                        Collection::Items
                    },
                    scope: Some(scope.clone()),
                },
                cursor.as_deref(),
                1,
                &budget,
            )
            .map_err(|e| {
                if e.code == "invalid_or_stale_cursor" {
                    "source_revision_changed"
                } else {
                    e.code
                }
            })?;
        if page.source_revision != entry.source_revision {
            return Err("source_revision_changed");
        }
        reasons.extend(page.issues);
        for r in page.records {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            records.push((json!({"kind":if phase=="context"{"observer_instructions"}else{"observer_item"},"raw":r.fields,"issues":r.issues}),position.clone()));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            if phase == "context" {
                phase = "items".into();
            } else {
                next = None;
                break;
            }
        }
        next = Some(json!({"phase":phase,"cursor":cursor}));
        if records.len() >= limit || (details && !records.is_empty()) {
            break;
        }
    }
    if skip > 0 {
        return Err("search_position_unavailable");
    }
    Ok(Window {
        records,
        next,
        reasons,
    })
}
// Search positions use source record identity, not re-redacted text or an
// ordinal that could shift when a saved window is expanded.
fn record_identity(record: &Value) -> String {
    let raw = &record["raw"];
    digest(
        &json!([
            record["kind"],
            raw["itemKey"],
            raw["requestId"],
            raw["responseId"],
            raw["captureSeq"],
            raw["key"],
            raw["source"],
            raw["viewSeq"],
            raw["code"],
            raw["afterViewSeq"],
            raw["throughViewSeq"],
            raw["segment"],
            raw["byteOffset"]
        ])
        .to_string(),
    )
}

fn text(value: &Value) -> String {
    for field in [
        "text",
        "summaryText",
        "message",
        "output",
        "arguments",
        "command",
    ] {
        if let Some(s) = value[field].as_str() {
            return s.into();
        }
    }
    for field in ["content", "summary"] {
        if let Some(values) = value[field].as_array() {
            let s = values
                .iter()
                .filter_map(|v| v["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}
fn preview(record: &Value) -> Value {
    let mut kind = record["kind"].as_str().unwrap_or("unknown").to_owned();
    let raw = &record["raw"];
    let mut content = text(record);
    let mut role = None;
    let mut label = String::new();
    if kind == "observer_item" {
        kind = raw["itemType"].as_str().unwrap_or("unknown").into();
        content = text(raw);
        if content.is_empty() {
            content = text(&raw["raw"]["payload"]);
        }
    } else if kind == "items" {
        kind = raw["kind"].as_str().unwrap_or("unknown").into();
        role = raw["author"]["role"].as_str();
        content = text(raw);
    }
    if matches!(kind.as_str(), "user_message" | "userMessage") {
        role = Some("user");
    }
    if matches!(kind.as_str(), "agent_message" | "agentMessage") {
        role = Some("assistant");
    }
    let payload = if raw["payload"].is_object() {
        &raw["payload"]
    } else {
        raw
    };
    if content.is_empty() {
        content = text(payload);
    }
    if matches!(kind.as_str(), "tool_call" | "toolCall") {
        if let Some(arguments) = payload["arguments"].as_str().or(raw["arguments"].as_str()) {
            content = serde_json::from_str::<Value>(arguments)
                .ok()
                .and_then(|v| {
                    v["cmd"]
                        .as_str()
                        .or(v["command"].as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| arguments.into());
        }
    } else if kind == "tool_output"
        && let Some(output) = payload["output"].as_str()
    {
        content = output.into();
    }
    if let Some(name) = payload["name"].as_str() {
        label = name.into();
    }
    let truncated = content.chars().count() > 12000
        || record["detailsOmitted"] == true
        || raw["truncated"] == true;
    let content = content.chars().take(12000).collect::<String>();
    json!({"kind":kind,"role":role,"text":content,"label":label,"status":payload["status"],"truncated":truncated,"offset":record["offset"],"issues":record["issues"],"model":raw["author"]["requestedModel"],"usage":if kind=="responses"{raw["usage"].clone()}else{Value::Null}})
}
