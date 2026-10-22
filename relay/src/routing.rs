//! Same-origin documents come exclusively from the relay build. A page grant is
//! only a selector: its exact parent portal cookie remains the authority.
use crate::{
    App, Failure, Result, bad, denied,
    store::{self, now},
};
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use codex_relay_protocol::WORKBENCH_API_VERSION;
use rusqlite::params;
use uuid::Uuid;

pub(crate) fn canonical_gateway(path: &str) -> Result<(Uuid, &str)> {
    let (id, tail) = path
        .strip_prefix("/g/")
        .and_then(|v| v.split_once('/'))
        .ok_or_else(bad)?;
    let uuid = Uuid::parse_str(id).map_err(|_| bad())?;
    if uuid.is_nil()
        || uuid.to_string() != id
        || tail.contains('%')
        || tail.contains("..")
        || tail.contains("//")
        || tail.contains('\\')
    {
        return Err(bad());
    }
    Ok((uuid, tail))
}

pub(crate) async fn page(State(s): State<App>, h: HeaderMap, req: Request) -> Result<Response> {
    let (gateway, tail) = canonical_gateway(req.uri().path())?;
    if !codex_relay_protocol::document_path(&format!("/{tail}")) {
        return Err(Failure(StatusCode::NOT_FOUND));
    }
    let parent = s.authorized_portal_session(&h)?;
    let owned: bool = s.0.store.0.lock().unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM gateways WHERE id=?1 AND account=?2)",
        params![gateway.to_string(), parent.account.to_string()],
        |r| r.get(0),
    )?;
    if !owned {
        return Err(Failure(StatusCode::NOT_FOUND));
    }
    central_asset("workbench.html")
}

pub(crate) async fn asset(Path(path): Path<String>) -> Result<Response> {
    if path.is_empty()
        || path.contains("..")
        || path.contains('%')
        || path.contains('\\')
        || path.contains("//")
    {
        return Err(bad());
    }
    central_asset(&format!("assets/{path}"))
}
fn central_asset(path: &str) -> Result<Response> {
    let asset =
        codex_workbench_assets::WorkbenchAssets::get(path).ok_or(Failure(StatusCode::NOT_FOUND))?;
    let content_type = if path == "workbench.html" {
        "text/html; charset=utf-8"
    } else {
        codex_workbench_assets::content_type(path).ok_or(Failure(StatusCode::NOT_FOUND))?
    };
    Ok((
        [("content-type", content_type)],
        Body::from(asset.data.into_owned()),
    )
        .into_response())
}

/// Preserve business query bytes, consuming exactly one canonical selector pair.
pub(crate) fn selectors(
    h: &HeaderMap,
    query: Option<&str>,
    streaming: bool,
) -> Result<(Uuid, String)> {
    let mut grant = None;
    let mut api = None;
    let mut remaining = Vec::new();
    for pair in query.unwrap_or("").split('&').filter(|v| !v.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decoded = url::form_urlencoded::parse(pair.as_bytes())
            .next()
            .ok_or_else(bad)?;
        if matches!(decoded.0.as_ref(), "relayGrant" | "relayApi") {
            if !streaming || key != decoded.0 || value != decoded.1 {
                return Err(bad());
            }
            let slot = if key == "relayGrant" {
                &mut grant
            } else {
                &mut api
            };
            if slot.replace(value.to_owned()).is_some() {
                return Err(bad());
            }
        } else {
            remaining.push(pair);
        }
    }
    for (name, slot) in [
        ("x-workbench-grant", &mut grant),
        ("x-workbench-api-version", &mut api),
    ] {
        let values = h.get_all(name).iter().collect::<Vec<_>>();
        if values.len() > 1 || (streaming && !values.is_empty()) {
            return Err(bad());
        }
        if let Some(v) = values.first()
            && slot
                .replace(v.to_str().map_err(|_| bad())?.to_owned())
                .is_some()
        {
            return Err(bad());
        }
    }
    if api.as_deref() != Some(&WORKBENCH_API_VERSION.to_string()) {
        return Err(Failure(StatusCode::CONFLICT));
    }
    let raw = grant.ok_or_else(denied)?;
    let grant = Uuid::parse_str(&raw).map_err(|_| bad())?;
    if grant.is_nil() || raw != grant.to_string() {
        return Err(bad());
    }
    Ok((grant, remaining.join("&")))
}

pub(crate) fn grant_session(
    s: &App,
    h: &HeaderMap,
    gateway: Uuid,
    grant: Uuid,
) -> Result<store::Session> {
    let mut parent = s.authorized_portal_session(h)?;
    let valid: bool = s.0.store.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM sessions c JOIN sessions p ON p.id=c.parent JOIN accounts a ON a.id=c.account JOIN gateways g ON g.id=c.gateway WHERE c.id=?1 AND c.parent=?2 AND c.gateway=?3 AND c.account=?4 AND p.account=c.account AND p.gateway IS NULL AND p.parent IS NULL AND c.expires>?5 AND p.expires>?5 AND a.disabled=0 AND a.must_change_password=0 AND c.auth_generation=a.auth_generation AND p.auth_generation=a.auth_generation AND g.account=a.id AND g.auth_generation=a.auth_generation AND c.ui_api_version=?6 AND g.workbench_api_version=?6)",params![grant.to_string(),parent.id.to_string(),gateway.to_string(),parent.account.to_string(),now(),WORKBENCH_API_VERSION],|r|r.get(0))?;
    if !valid {
        return Err(denied());
    }
    parent.id = grant;
    parent.gateway = Some(gateway);
    Ok(parent)
}
