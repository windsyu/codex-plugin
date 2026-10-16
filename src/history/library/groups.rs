use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupQuery {
    pub project_id: Option<String>,
    pub source_id: Option<String>,
    pub kind: Option<String>,
    pub q: Option<String>,
    pub section: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupAnchorQuery {
    pub anchor_entry_id: String,
    pub revision: String,
}
#[derive(Clone)]
struct Candidate {
    entry: Entry,
    locator: Value,
    matches: bool,
}
fn home(source: &Source) -> Option<PathBuf> {
    let path = match source {
        Source::Native { codex_home, .. } => codex_home,
        Source::Observer { native_home, .. } => native_home.as_ref()?,
        Source::Workbench { .. } => return None,
    };
    Some(path.canonicalize().unwrap_or_else(|_| path.clone()))
}
fn reliable(source: &Source, candidate: &Candidate) -> bool {
    let entry = &candidate.entry;
    let key = match source {
        Source::Native { .. } => entry.native_thread_id.as_deref(),
        Source::Observer { .. } => candidate.locator["thread"].as_str(),
        Source::Workbench { .. } => candidate.locator["run"].as_str(),
    };
    key.is_some_and(|k| {
        !k.is_empty() && k.len() <= 4096 && Entry::new(source, k).entry_id == entry.entry_id
    }) && entry.source_identity == source.identity()
        && entry.kind == source.kind()
        && !matches!(
            entry.kind_basis.as_str(),
            "metadata_conflict_v1" | "metadata_invalid_v1"
        )
        && !entry.coverage.reasons.iter().any(|r| {
            r.contains("identity")
                || r == "conflicting_session_metadata"
                || r == "conflicting_conversation_metadata"
                || r.contains("threadKey")
                || r.contains("codexThreadId")
        })
}
fn group_ids(sources: &[Source], entries: &[Candidate]) -> Vec<String> {
    let homes: HashMap<_, _> = sources.iter().map(|s| (s.id(), home(s))).collect();
    let source_by_id: HashMap<_, _> = sources.iter().map(|s| (s.id(), s)).collect();
    let mut buckets = HashMap::<(PathBuf, String), Vec<usize>>::new();
    let mut ids: Vec<_> = entries
        .iter()
        .map(|c| format!("g_{}", digest(&c.entry.entry_id)))
        .collect();
    for (i, c) in entries.iter().enumerate() {
        if c.entry.kind == "workbench" {
            continue;
        }
        let Some(thread) = c
            .entry
            .native_thread_id
            .as_deref()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .filter(|v| !v.is_nil())
        else {
            continue;
        };
        let Some(Some(home)) = homes.get(c.entry.source_id.as_str()) else {
            continue;
        };
        buckets
            .entry((home.clone(), thread.to_string()))
            .or_default()
            .push(i);
    }
    for ((home, thread), indexes) in buckets {
        let first = &entries[indexes[0]].entry;
        let mut seen = HashSet::new();
        let qualified = indexes.len() <= 10
            && indexes.iter().all(|i| {
                let c = &entries[*i];
                let e = &c.entry;
                seen.insert(&e.source_id)
                    && source_by_id
                        .get(e.source_id.as_str())
                        .is_some_and(|s| reliable(s, c))
                    && e.project_id == first.project_id
                    && e.project_path == first.project_path
                    && e.conversation_kind == first.conversation_kind
            });
        if qualified {
            let id = format!(
                "g_{}",
                digest(
                    &json!([
                        home,
                        thread,
                        first.project_id,
                        first.project_path,
                        first.conversation_kind
                    ])
                    .to_string()
                )
            );
            for i in indexes {
                ids[i] = id.clone();
            }
        }
    }
    ids
}
fn section(entry: &Entry) -> &'static str {
    if entry.kind == "workbench" {
        "runs"
    } else if matches!(
        entry.conversation_kind.as_str(),
        "subagent" | "background_review"
    ) {
        "auxiliary"
    } else {
        "primary"
    }
}
fn priority(member: &Value) -> (bool, bool, u8, &str) {
    (
        !member["capabilities"]["read"].as_bool().unwrap_or(false),
        member.get("match").is_none(),
        match member["kind"].as_str() {
            Some("native") => 0,
            Some("observer") => 1,
            _ => 2,
        },
        member["entryId"].as_str().unwrap_or(""),
    )
}
fn member(entry: &Entry) -> Value {
    json!({"entryId":entry.entry_id,"sourceId":entry.source_id,"sourceIdentity":entry.source_identity,"kind":entry.kind,"sourceRevision":entry.source_revision,"title":entry.title,"preview":entry.preview,"coverage":entry.coverage,"capabilities":entry.capabilities,"conversationKind":entry.conversation_kind,"parentThreadId":entry.parent_thread_id,"agentName":entry.agent_name})
}
fn group(id: &str, candidates: &[&Candidate]) -> Value {
    let first = &candidates[0].entry;
    let mut members: Vec<_> = candidates.iter().map(|c| member(&c.entry)).collect();
    members.sort_by(|a, b| priority(a).cmp(&priority(b)));
    json!({"groupId":id,"section":section(first),"conversationKind":first.conversation_kind,"preferredEntryId":members[0]["entryId"],"members":members,"projectId":first.project_id,"projectPath":first.project_path,"latestRecordedAt":candidates.iter().filter_map(|c|c.entry.recorded_at.as_ref()).max()})
}
fn validate(query: &GroupQuery) -> Result<()> {
    if query.q.as_ref().is_some_and(|s| s.len() > 512)
        || query.cursor.as_ref().is_some_and(|s| s.len() > 4096)
        || query.limit.is_some_and(|v| v == 0 || v > 200)
        || query
            .section
            .as_ref()
            .is_some_and(|s| !matches!(s.as_str(), "primary" | "auxiliary" | "runs"))
        || query
            .kind
            .as_ref()
            .is_some_and(|s| !matches!(s.as_str(), "native" | "observer" | "workbench"))
        || query.project_id.as_ref().is_some_and(|s| s.len() > 128)
        || query.source_id.as_ref().is_some_and(|s| s.len() > 64)
    {
        return Err("invalid_query");
    }
    Ok(())
}
pub(super) fn revisions(
    db: &Connection,
    shared: &Shared,
    sources: &[Source],
    organization_revision: &str,
) -> Result<(String, String, Vec<String>)> {
    let mut generations = vec![];
    let mut revisions = vec![];
    let mut reasons = shared
        .states(sources)
        .iter()
        .filter(|s| matches!(s.state.as_str(), "unavailable" | "indexing" | "partial"))
        .map(|s| format!("source_{}:{}", s.state, s.id))
        .collect::<Vec<_>>();
    for source in sources {
        let row:Option<(String,String,String)>=db.query_row("SELECT generation,revision,issues FROM catalog_sources WHERE id=?1 AND identity=?2",params![source.id(),source.identity()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|"cache_unavailable")?;
        if let Some((g, r, issues)) = row {
            generations.push(g);
            revisions.push(r);
            reasons
                .extend(serde_json::from_str::<Vec<String>>(&issues).map_err(|_| "cache_invalid")?);
        } else {
            reasons.push("indexing".into());
        }
    }
    reasons.sort();
    reasons.dedup();
    Ok((
        serde_json::to_string(&generations).unwrap(),
        digest(
            &json!([
                CATALOG_VERSION,
                "groups_v1",
                revisions,
                sources,
                sources.iter().map(home).collect::<Vec<_>>(),
                organization_revision
            ])
            .to_string(),
        ),
        reasons,
    ))
}
fn candidates(
    db: &Connection,
    generations: &str,
    q: &str,
    anchor: Option<&Entry>,
    sources: &[Source],
) -> Result<Vec<Candidate>> {
    // Full-group lookup reads only metadata/locators for the anchor's identity bucket.
    let associated: Vec<_> = anchor
        .map(|a| {
            let own = sources
                .iter()
                .find(|s| s.id() == a.source_id)
                .and_then(home);
            sources
                .iter()
                .filter(|s| own.is_some() && home(s) == own)
                .map(Source::id)
                .collect()
        })
        .unwrap_or_default();
    let sql = if anchor.is_some() {
        "SELECT e.metadata,l.locator,1 FROM catalog_entries e LEFT JOIN catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.generation IN(SELECT value FROM json_each(?1)) AND (e.id=?3 OR (replace(replace(replace(replace(lower(json_extract(e.metadata,'$.nativeThreadId')),'urn:uuid:',''),'-',''),'{',''),'}','')=?4 AND e.source IN(SELECT value FROM json_each(?5)))) LIMIT 100001"
    } else {
        "SELECT e.metadata,l.locator,(?2='' OR instr(lower(e.metadata||e.search),lower(?2))>0) FROM catalog_entries e LEFT JOIN catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.generation IN(SELECT value FROM json_each(?1)) LIMIT 100001"
    };
    let mut stmt = db.prepare(sql).map_err(|_| "cache_unavailable")?;
    let associated = serde_json::to_string(&associated).unwrap();
    let read = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, bool>(2)?,
        ))
    };
    let mut rows = if let Some(a) = anchor {
        stmt.query_map(
            params![
                generations,
                q,
                a.entry_id,
                a.native_thread_id
                    .as_deref()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .map(|v| v.simple().to_string()),
                associated
            ],
            read,
        )
    } else {
        stmt.query_map(params![generations, q], read)
    }
    .map_err(|_| "cache_unavailable")?;
    let mut entries = vec![];
    let mut bytes = 0;
    for row in &mut rows {
        let (metadata, locator, matches) = row.map_err(|_| "cache_unavailable")?;
        bytes += metadata.len() + locator.as_ref().map_or(0, String::len);
        if entries.len() >= 100_000 || bytes > 64 * 1024 * 1024 {
            return Err("query_budget_exceeded");
        }
        entries.push(Candidate {
            entry: serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?,
            locator: serde_json::from_str(locator.as_deref().unwrap_or("null"))
                .map_err(|_| "cache_invalid")?,
            matches,
        });
    }
    Ok(entries)
}
fn offset(shared: &Shared, cursor: Option<&str>, revision: &str, binding: &str) -> Result<usize> {
    let Some(cursor) = cursor else { return Ok(0) };
    let value: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| "invalid_cursor")?,
    )
    .map_err(|_| "invalid_cursor")?;
    let payload = json!([value["revision"], value["binding"], value["offset"]]).to_string();
    if value["signature"]
        != blake3::keyed_hash(&shared.key, payload.as_bytes())
            .to_hex()
            .to_string()
    {
        return Err("invalid_cursor");
    }
    if value["revision"] != revision || value["binding"] != binding {
        return Err("stale_cursor");
    }
    value["offset"]
        .as_u64()
        .filter(|n| *n <= 100_000)
        .map(|v| v as usize)
        .ok_or("invalid_cursor")
}
pub(super) fn list(shared: &Shared, sources: &[Source], mut query: GroupQuery) -> Result<Value> {
    validate(&query)?;
    let cursor = query.cursor.take();
    let binding = digest(&serde_json::to_string(&query).unwrap());
    let (db, snapshot) = organization_view::open(shared, sources)?;
    let Some(db) = db else {
        if cursor.is_some() {
            return Err("stale_cursor");
        }
        return Ok(
            json!({"revision":"pending","organizationRevision":"absent","records":[],"nextCursor":null,"coverage":{"state":"partial","reasons":["indexing"]},"selectedProject":query.project_id.as_deref().filter(|p|*p!="unassigned").map(|p|json!({"projectId":p,"path":null,"status":"indexing"})),"counts":{"primary":{"groups":0,"records":0},"auxiliary":{"groups":0,"records":0},"runs":{"groups":0,"records":0}}}),
        );
    };
    let (generations, revision, reasons) = revisions(&db, shared, sources, &snapshot.revision)?;
    let offset = offset(shared, cursor.as_deref(), &revision, &binding)?;
    let q = query.q.as_deref().unwrap_or("");
    let entries = candidates(&db, &generations, q, None, sources)?;
    let ids = group_ids(sources, &entries);
    let mut buckets = BTreeMap::<String, Vec<&Candidate>>::new();
    for (id, c) in ids.iter().zip(&entries) {
        let e = &c.entry;
        if !c.matches
            || query.source_id.as_ref().is_some_and(|s| s != &e.source_id)
            || query.kind.as_ref().is_some_and(|s| s != &e.kind)
            || query.project_id.as_ref().is_some_and(|s| {
                if s == "unassigned" {
                    e.project_id.is_some()
                } else {
                    e.project_id.as_ref() != Some(s)
                }
            })
        {
            continue;
        }
        buckets.entry(id.clone()).or_default().push(c);
    }
    let mut counts = json!({"primary":{"groups":0,"records":0},"auxiliary":{"groups":0,"records":0},"runs":{"groups":0,"records":0}});
    let mut groups = vec![];
    for (id, members) in buckets {
        let section = section(&members[0].entry);
        counts[section]["groups"] = json!(counts[section]["groups"].as_u64().unwrap() + 1);
        counts[section]["records"] =
            json!(counts[section]["records"].as_u64().unwrap() + members.len() as u64);
        if section == query.section.as_deref().unwrap_or("primary") {
            let latest = members
                .iter()
                .filter_map(|c| c.entry.recorded_at.as_ref())
                .max();
            groups.push((id, members, latest));
        }
    }
    groups.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let total = groups.len();
    let mut records = vec![];
    let mut bytes = 0;
    for (id, members, _) in groups
        .into_iter()
        .skip(offset)
        .take(query.limit.unwrap_or(30))
    {
        let mut value = group(&id, &members);
        if !q.is_empty() {
            for m in value["members"].as_array_mut().unwrap() {
                let hit:Option<(u64,String)>=db.query_row("SELECT seq,body FROM catalog_records WHERE generation IN(SELECT value FROM json_each(?1)) AND entry=?2 AND instr(lower(body),lower(?3))>0 ORDER BY seq LIMIT 1",params![generations,m["entryId"].as_str(),q],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|"cache_unavailable")?;
                if let Some((seq, body)) = hit {
                    let r: Value = serde_json::from_str(&body).map_err(|_| "cache_invalid")?;
                    m["match"] = json!({"record":seq,"text":r["text"].as_str().or(r["raw"]["summaryText"].as_str()).unwrap_or("上下文或详情中包含搜索内容").chars().take(200).collect::<String>(),"sourceRevision":m["sourceRevision"]});
                }
            }
            let members = value["members"].as_array_mut().unwrap();
            members.sort_by(|a, b| priority(a).cmp(&priority(b)));
            value["preferredEntryId"] = value["members"][0]["entryId"].clone();
        }
        let size = value.to_string().len();
        if size > 900 * 1024 {
            return Err("record_too_large");
        }
        if bytes + size > 900 * 1024 {
            break;
        }
        bytes += size;
        records.push(value);
    }
    let next = offset + records.len();
    let next_cursor=(next<total).then(||{
        let payload=json!([revision,binding,next]).to_string();URL_SAFE_NO_PAD.encode(json!({"revision":revision,"binding":binding,"offset":next,"signature":blake3::keyed_hash(&shared.key,payload.as_bytes()).to_hex().to_string()}).to_string())
    });
    let mut result = json!({"revision":revision,"organizationRevision":snapshot.revision,"records":records,"nextCursor":next_cursor,"coverage":{"state":if reasons.is_empty(){"complete_for_source"}else{"partial"},"reasons":reasons},"counts":counts,"selectedProject":null});
    selected_project(
        &db,
        shared,
        sources,
        query.project_id.as_deref(),
        &mut result,
    )?;
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    Ok(result)
}
pub(super) fn full(
    shared: &Shared,
    sources: &[Source],
    id: String,
    query: GroupAnchorQuery,
) -> Result<Value> {
    if id.len() > 128 || query.anchor_entry_id.len() > 128 || query.revision.len() > 128 {
        return Err("invalid_query");
    }
    let (db, snapshot) = organization_view::open(shared, sources)?;
    let db = db.ok_or("group_unavailable")?;
    let (generations, revision, _) = revisions(&db, shared, sources, &snapshot.revision)?;
    let metadata:Option<String>=db.query_row("SELECT metadata FROM catalog_entries WHERE generation IN(SELECT value FROM json_each(?1)) AND id=?2",params![generations,query.anchor_entry_id],|r|r.get(0)).optional().map_err(|_|"cache_unavailable")?;
    let anchor: Entry =
        serde_json::from_str(&metadata.ok_or("group_unavailable")?).map_err(|_| "cache_invalid")?;
    if revision != query.revision {
        return Err("group_changed");
    }
    let entries = candidates(&db, &generations, "", Some(&anchor), sources)?;
    let ids = group_ids(sources, &entries);
    let anchor_id = entries
        .iter()
        .position(|c| c.entry.entry_id == anchor.entry_id)
        .ok_or("group_unavailable")?;
    if ids[anchor_id] != id {
        return Err("group_changed");
    }
    let members: Vec<_> = entries
        .iter()
        .zip(ids)
        .filter_map(|(c, g)| (g == id).then_some(c))
        .collect();
    let result = group(&id, &members);
    if result.to_string().len() > 900 * 1024 {
        return Err("record_too_large");
    }
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    Ok(result)
}
#[cfg(test)]
#[path = "groups_tests.rs"]
mod tests;
