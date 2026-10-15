//! Effective display assignment; never modifies the durable source or launch cwd.
use super::*;
use organization::{Override, Snapshot};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OrganizationWrite {
    pub instance_id: uuid::Uuid,
    pub source_identity: String,
    pub source_revision: String,
    pub action: String,
    pub project_id: Option<String>,
}
fn binding(entry: &Entry, locator: &Value, sources: &[Source]) -> Option<String> {
    let source = sources.iter().find(|s| {
        s.id() == entry.source_id && s.identity() == entry.source_identity && s.kind() == entry.kind
    })?;
    let primary = match source {
        Source::Native { .. } => entry
            .native_thread_id
            .as_deref()
            .filter(|v| uuid::Uuid::parse_str(v).is_ok_and(|u| !u.is_nil())),
        Source::Observer { .. } => locator["thread"].as_str(),
        Source::Workbench { .. } => locator["run"].as_str(),
    }?;
    if primary.is_empty()
        || primary.len() > 4096
        || Entry::new(source, primary).entry_id != entry.entry_id
        || entry
            .coverage
            .reasons
            .iter()
            .any(|r| r.contains("identity") || r.contains("threadKey"))
    {
        return None;
    }
    Some(digest(
        &json!([entry.source_identity, entry.kind, primary]).to_string(),
    ))
}
fn describe(entry: &Value, automatic: &Value, state: &str) -> Value {
    let reason = if entry["projectBasis"] == "user_unassigned" {
        Some("user_unassigned")
    } else {
        unassigned_reason(entry)
    };
    json!({"automatic":{"projectId":automatic["projectId"],"projectPath":automatic["projectPath"],"basis":automatic["projectBasis"]},"effective":{"projectId":entry["projectId"],"projectPath":entry["projectPath"],"basis":if state=="applied" {entry["projectBasis"].as_str().unwrap_or("automatic")}else{"automatic"}},"unassignedReason":reason,"conversationKind":entry["conversationKind"],"kindBasis":entry["kindBasis"],"overrideState":state})
}
pub(super) fn decorate(entry: &mut Value) {
    if entry.get("organization").is_none_or(Value::is_null) {
        entry["organization"] = describe(entry, entry, "none");
    }
}
pub(super) fn unavailable(entry: &mut Value) {
    entry["organization"] = describe(entry, entry, "unavailable");
}
// Only bounded user state is materialized. The catalog remains read-only; the
// temporary view shadows its name on this connection, never on index workers.
pub(super) fn prepare(db: &Connection, sources: &[Source], snapshot: &Snapshot) -> Result<()> {
    if snapshot.overrides.is_empty() {
        return Ok(());
    }
    db.execute_batch("PRAGMA query_only=OFF; CREATE TEMP TABLE effective_overrides(id TEXT PRIMARY KEY, project TEXT,metadata TEXT NOT NULL); CREATE TEMP VIEW catalog_entries AS SELECT e.generation,e.id,e.source,CASE WHEN o.id IS NULL THEN e.project ELSE o.project END project,e.kind,e.time,coalesce(o.metadata,e.metadata) metadata,e.search FROM main.catalog_entries e LEFT JOIN effective_overrides o ON o.id=e.id;").map_err(|_|"organization_unavailable")?;
    let identities =
        serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>()).unwrap();
    let mut lookup=db.prepare("SELECT e.metadata,l.locator FROM main.catalog_entries e JOIN main.catalog_sources s ON s.generation=e.generation JOIN main.catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.id=?1 AND s.identity IN(SELECT value FROM json_each(?2))").map_err(|_|"cache_unavailable")?;
    let mut insert = db
        .prepare("INSERT INTO effective_overrides VALUES (?1,?2,?3)")
        .map_err(|_| "organization_unavailable")?;
    for (id, o) in &snapshot.overrides {
        let found: Option<(String, String)> = lookup
            .query_row(params![id, identities], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(|_| "cache_unavailable")?;
        let Some((metadata, locator)) = found else {
            continue;
        };
        let entry: Entry = serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?;
        let locator: Value = serde_json::from_str(&locator).map_err(|_| "cache_invalid")?;
        let automatic = serde_json::to_value(&entry).unwrap();
        let mut effective = automatic.clone();
        let applied = entry.source_identity == o.source_identity
            && binding(&entry, &locator, sources).as_deref() == Some(&o.identity_digest);
        if applied {
            effective["projectId"] = json!(o.project_id);
            effective["projectPath"] = json!(o.project_path);
            effective["projectBasis"] = json!(if o.disposition == "project" {
                "user_assigned"
            } else {
                "user_unassigned"
            });
        }
        effective["organization"] = describe(
            &effective,
            &automatic,
            if applied { "applied" } else { "stale" },
        );
        insert
            .execute(params![
                id,
                effective["projectId"].as_str(),
                effective.to_string()
            ])
            .map_err(|_| "organization_unavailable")?;
    }
    db.execute_batch("PRAGMA query_only=ON")
        .map_err(|_| "organization_unavailable")?;
    Ok(())
}
pub(super) fn open(shared: &Shared, sources: &[Source]) -> Result<(Option<Connection>, Snapshot)> {
    let snapshot = shared.organization.snapshot()?;
    let db = read_db(shared)?;
    if let Some(db) = &db {
        db.execute_batch("BEGIN DEFERRED")
            .map_err(|_| "cache_busy")?;
        prepare(db, sources, &snapshot)?;
    }
    Ok((db, snapshot))
}
pub(super) fn open_source(
    shared: &Shared,
    sources: &[Source],
) -> Result<(Option<Connection>, Snapshot)> {
    match open(shared, sources) {
        Ok(value) => Ok(value),
        Err(
            "organization_unavailable" | "unsafe_organization" | "organization_budget_exceeded",
        ) => {
            let db = read_db(shared)?;
            if let Some(db) = &db {
                db.execute_batch("BEGIN DEFERRED")
                    .map_err(|_| "cache_busy")?;
            }
            Ok((
                db,
                Snapshot {
                    revision: "unavailable".into(),
                    overrides: Default::default(),
                },
            ))
        }
        Err(error) => Err(error),
    }
}
fn entry(db: &Connection, id: &str, sources: &[Source]) -> Result<(Entry, Value)> {
    let identities =
        serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>()).unwrap();
    let found:Option<(String,String)>=db.query_row("SELECT e.metadata,l.locator FROM main.catalog_entries e JOIN main.catalog_sources s ON s.generation=e.generation JOIN main.catalog_locators l ON l.generation=e.generation AND l.entry=e.id WHERE e.id=?1 AND s.identity IN(SELECT value FROM json_each(?2))",params![id,identities],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|"cache_unavailable")?;
    let (metadata, locator) = found.ok_or("entry_unavailable")?;
    Ok((
        serde_json::from_str(&metadata).map_err(|_| "cache_invalid")?,
        serde_json::from_str(&locator).map_err(|_| "cache_invalid")?,
    ))
}
fn response(
    db: &Connection,
    shared: &Shared,
    sources: &[Source],
    snapshot: &Snapshot,
    id: &str,
) -> Result<Value> {
    let (original, locator) = entry(db, id, sources)?;
    let raw:String=db.query_row("SELECT metadata FROM catalog_entries WHERE id=?1 AND generation IN(SELECT generation FROM catalog_sources WHERE identity IN(SELECT value FROM json_each(?2)))",params![id,serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>()).unwrap()],|r|r.get(0)).map_err(|_|"entry_unavailable")?;
    let mut effective: Value = serde_json::from_str(&raw).map_err(|_| "cache_invalid")?;
    decorate(&mut effective);
    let revision = super::groups::revisions(db, shared, sources, &snapshot.revision)?.1;
    let can_adjust = binding(&original, &locator, sources).is_some();
    Ok(
        json!({"organization":effective["organization"],"organizationRevision":snapshot.revision,"revision":revision,"canAdjust":can_adjust,"adjustmentReason":if can_adjust{None}else{Some("identity_unconfirmed")}}),
    )
}
pub(super) fn read(shared: &Shared, sources: &[Source], id: String) -> Result<Value> {
    if id.len() > 128 {
        return Err("invalid_query");
    }
    let (db, snapshot) = open(shared, sources)?;
    let db = db.ok_or("entry_unavailable")?;
    let result = response(&db, shared, sources, &snapshot, &id)?;
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    Ok(result)
}
pub(super) fn save(
    shared: &Shared,
    sources: &[Source],
    id: String,
    expected: String,
    request: OrganizationWrite,
) -> Result<Value> {
    if id.len() > 128
        || expected.len() > 128
        || request.source_identity.len() > 128
        || request.source_revision.len() > 256
        || !matches!(
            request.action.as_str(),
            "project" | "unassigned" | "automatic"
        )
        || (request.action == "project") != request.project_id.is_some()
        || request
            .project_id
            .as_ref()
            .is_some_and(|p| p.len() > 128 || p == "unassigned")
    {
        return Err("invalid_query");
    }
    let validate = |snapshot: &Snapshot| {
        let db = read_db(shared)?.ok_or("entry_unavailable")?;
        db.execute_batch("BEGIN DEFERRED")
            .map_err(|_| "cache_busy")?;
        let (original, locator) = entry(&db, &id, sources)?;
        if original.source_identity != request.source_identity
            || original.source_revision != request.source_revision
        {
            return Err("source_revision_changed");
        }
        let identity_digest =
            binding(&original, &locator, sources).ok_or("organization_identity_unconfirmed")?;
        prepare(&db, sources, snapshot)?;
        let target = if request.action == "project" {
            let identities =
                serde_json::to_string(&sources.iter().map(Source::identity).collect::<Vec<_>>())
                    .unwrap();
            let path:Option<String>=db.query_row("SELECT min(json_extract(e.metadata,'$.projectPath')) FROM catalog_entries e JOIN catalog_sources s ON s.generation=e.generation WHERE e.project=?1 AND s.identity IN(SELECT value FROM json_each(?2))",params![request.project_id,identities],|r|r.get(0)).map_err(|_|"cache_unavailable")?;
            Some(path.ok_or("project_unavailable")?)
        } else {
            None
        };
        if shared.policy()?.1 != sources {
            return Err("source_revoked");
        }
        Ok((request.action != "automatic").then(|| Override {
            source_identity: original.source_identity,
            identity_digest,
            disposition: request.action.clone(),
            project_id: request.project_id.clone(),
            project_path: target,
        }))
    };
    // Reject malformed/unauthorized targets before first-create side effects,
    // then repeat validation inside the transaction to close the race.
    let preflight = shared.organization.snapshot()?;
    if preflight.revision != expected {
        return Err("organization_changed");
    }
    validate(&preflight)?;
    let snapshot = shared.organization.save(&expected, &id, validate)?;
    // A commit/ACK race is resolved by reading again, never by blindly replaying a write.
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    let db = read_db(shared)?.ok_or("entry_unavailable")?;
    db.execute_batch("BEGIN DEFERRED")
        .map_err(|_| "cache_busy")?;
    prepare(&db, sources, &snapshot)?;
    let result = response(&db, shared, sources, &snapshot, &id)?;
    if shared.policy()?.1 != sources {
        return Err("source_revoked");
    }
    Ok(result)
}
