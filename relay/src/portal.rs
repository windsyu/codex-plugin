use crate::{
    App, Failure, Result,
    auth::set_cookie,
    bad, denied,
    store::{digest, now, secret},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use codex_relay_protocol::{BindingPending, BindingPoll, BindingStart, Bound};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use uuid::Uuid;

pub(crate) struct Pending {
    device_hash: String,
    user_code: String,
    name: String,
    expires: i64,
    bound: Option<Bound>,
}
pub(crate) async fn index(
    State(s): State<App>,
    h: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    if s.0.config.host_origin(&h).is_some_and(|(_, g)| g.is_some()) {
        tunnel_index(s, h, request).await
    } else {
        Html(include_str!("../static/index.html")).into_response()
    }
}
async fn tunnel_index(s: App, h: HeaderMap, request: axum::extract::Request) -> Response {
    crate::tunnel::forward_inner(s, h, request, None)
        .await
        .unwrap_or_else(IntoResponse::into_response)
}
pub(crate) async fn script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("../static/portal.js"),
    )
}
pub(crate) async fn exchange_page() -> impl IntoResponse {
    Html(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Opening workbench</title><p id=\"status\">Opening your workbench…</p><script src=\"/relay/v1/exchange.js\" defer></script></html>",
    )
}
pub(crate) async fn exchange_script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        "const code=new URLSearchParams(location.hash.slice(1)).get('code');history.replaceState(null,'',location.pathname);fetch('/relay/v1/session/exchange',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({code})}).then(r=>{if(!r.ok)throw Error();location.replace('/')}).catch(()=>{document.getElementById('status').textContent='This link expired or was already used. Open the workbench again from your account.'});",
    )
}
pub(crate) async fn binding_start(
    State(s): State<App>,
    Json(input): Json<BindingStart>,
) -> Result<Json<BindingPending>> {
    s.rate("binding-start".into(), 30)?;
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.name.chars().any(char::is_control)
    {
        return Err(bad());
    }
    let id = Uuid::new_v4();
    let device = secret();
    let user = Uuid::new_v4().simple().to_string()[..10].to_ascii_uppercase();
    let mut b = s.0.bindings.lock().unwrap();
    b.retain(|_, v| v.expires > now());
    if b.len() >= 1024 {
        return Err(Failure(StatusCode::TOO_MANY_REQUESTS));
    }
    b.insert(
        id,
        Pending {
            device_hash: digest(&device),
            user_code: user.clone(),
            name: input.name,
            expires: now() + 600,
            bound: None,
        },
    );
    Ok(Json(BindingPending {
        binding_id: id,
        device_code: device,
        user_code: user.clone(),
        verification_url: format!("{}/#bind={user}", s.0.config.portal_origin),
        expires_in_seconds: 600,
        interval: 3,
    }))
}
pub(crate) async fn binding_poll(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Json(input): Json<BindingPoll>,
) -> Result<Response> {
    s.rate(format!("binding-poll:{id}"), 30)?;
    let mut b = s.0.bindings.lock().unwrap();
    let item = b.get_mut(&id).ok_or_else(denied)?;
    if item.expires <= now() || item.device_hash != digest(&input.device_code) {
        return Err(denied());
    }
    if let Some(bound) = item.bound.take() {
        b.remove(&id);
        if s.0.store.gateway_auth(&bound.credential).is_none() {
            return Err(denied());
        }
        return Ok(Json(bound).into_response());
    }
    Ok(StatusCode::ACCEPTED.into_response())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Approval {
    user_code: String,
}
pub(crate) async fn binding_preview(
    State(s): State<App>,
    h: HeaderMap,
    Json(input): Json<Approval>,
) -> Result<Json<serde_json::Value>> {
    let account = s.portal_session(&h)?;
    s.rate(format!("approve:{}", account.account), 20)?;
    let b = s.0.bindings.lock().unwrap();
    let p = b
        .values()
        .find(|p| p.user_code == input.user_code && p.expires > now() && p.bound.is_none())
        .ok_or_else(denied)?;
    Ok(Json(serde_json::json!({"name":p.name})))
}
pub(crate) async fn binding_approve(
    State(s): State<App>,
    h: HeaderMap,
    Json(input): Json<Approval>,
) -> Result<Json<serde_json::Value>> {
    let account = s.portal_session(&h)?;
    s.rate(format!("approve:{}", account.account), 20)?;
    let mut b = s.0.bindings.lock().unwrap();
    let p = b
        .values_mut()
        .find(|p| p.user_code == input.user_code && p.expires > now() && p.bound.is_none())
        .ok_or_else(denied)?;
    let id = s.0.config.fixed_gateway_id.unwrap_or_else(Uuid::new_v4);
    let gateway_origin = s.0.config.gateway_origin(id).ok_or_else(bad)?;
    let token = secret();
    let mut c = s.0.store.0.lock().unwrap();
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if s.0.config.fixed_gateway_id.is_some()
        && tx.query_row("SELECT EXISTS(SELECT 1 FROM gateways)", [], |r| {
            r.get::<_, bool>(0)
        })?
    {
        return Err(Failure(StatusCode::CONFLICT));
    }
    let n=tx.execute("INSERT INTO gateways(id,account,name,credential) SELECT ?1,id,?2,?3 FROM accounts WHERE id=?4 AND disabled=0",params![id.to_string(),p.name,digest(&token),account.account.to_string()])?;
    if n != 1 {
        return Err(denied());
    }
    crate::store::audit(
        &tx,
        "gateway_bound",
        Some(&account.account.to_string()),
        Some(&id.to_string()),
    )?;
    tx.commit()?;
    p.bound = Some(Bound {
        account_id: account.account,
        gateway_id: id,
        credential: token,
        gateway_origin,
    });
    Ok(Json(serde_json::json!({"gatewayId":id})))
}
pub(crate) async fn gateways(
    State(s): State<App>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let account = s.portal_session(&h)?;
    let c = s.0.store.0.lock().unwrap();
    let mut q =
        c.prepare("SELECT id,name FROM gateways WHERE account=?1 ORDER BY name,id LIMIT 1000")?;
    let rows = q
        .query_map([account.account.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    drop(c);
    let tunnels = s.0.tunnels.lock().unwrap();
    Ok(Json(
        serde_json::json!({"gateways":rows.into_iter().map(|(id,name)|{let uuid=Uuid::parse_str(&id).unwrap();serde_json::json!({"gatewayId":id,"name":name,"online":tunnels.contains_key(&uuid)})}).collect::<Vec<_>>()}),
    ))
}
pub(crate) async fn forget(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<StatusCode> {
    let account = s.portal_session(&h)?;
    {
        // Match control admission's lock order so a rebind cannot install a new
        // connection between deleting the old identity and removing its tunnel.
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        let n = tx.execute(
            "DELETE FROM gateways WHERE id=?1 AND account=?2",
            params![id.to_string(), account.account.to_string()],
        )?;
        if n != 1 {
            return Err(Failure(StatusCode::NOT_FOUND));
        }
        crate::store::audit(
            &tx,
            "gateway_forgotten",
            Some(&account.account.to_string()),
            Some(&id.to_string()),
        )?;
        tx.commit()?;
        if let Some(c) = tunnels.remove(&id) {
            c.cancel.send_replace(true);
        }
    }
    s.revoke_invalid();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeCredential {}
pub(crate) async fn revoke_credential(
    State(s): State<App>,
    h: HeaderMap,
    Json(_input): Json<RevokeCredential>,
) -> Result<StatusCode> {
    if h.get_all("authorization").iter().count() != 1 {
        return Err(denied());
    }
    let credential = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() <= 128)
        .ok_or_else(denied)?;
    let (gateway, account) = s.0.store.gateway_auth(credential).ok_or_else(denied)?;
    s.rate(format!("credential-revoke:{gateway}"), 3)?;
    {
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "DELETE FROM gateways WHERE id=?1 AND account=?2 AND credential=?3",
            params![gateway.to_string(), account.to_string(), digest(credential)],
        )?;
        if changed != 1 {
            return Err(denied());
        }
        crate::store::audit(
            &tx,
            "gateway_forgotten",
            Some(&account.to_string()),
            Some(&gateway.to_string()),
        )?;
        tx.commit()?;
        if let Some(c) = tunnels.remove(&gateway) {
            c.cancel.send_replace(true);
        }
    }
    s.revoke_invalid();
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn open(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let account = s.portal_session(&h)?;
    let gateway_origin =
        s.0.config
            .gateway_origin(id)
            .ok_or(Failure(StatusCode::NOT_FOUND))?;
    let code = secret();
    let c = s.0.store.0.lock().unwrap();
    let n=c.execute("INSERT INTO open_codes(hash,parent,gateway,expires) SELECT ?1,?2,id,?3 FROM gateways WHERE id=?4 AND account=?5",params![digest(&code),account.id.to_string(),now()+60,id.to_string(),account.account.to_string()])?;
    if n != 1 {
        return Err(Failure(StatusCode::NOT_FOUND));
    }
    Ok(Json(
        serde_json::json!({"url":format!("{}/relay/v1/open#code={code}",gateway_origin)}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Exchange {
    code: String,
}
pub(crate) async fn exchange(
    State(s): State<App>,
    h: HeaderMap,
    Json(input): Json<Exchange>,
) -> Result<Response> {
    let gateway =
        s.0.config
            .host_origin(&h)
            .and_then(|(_, g)| g)
            .ok_or_else(denied)?;
    if input.code.len() > 128 {
        return Err(denied());
    }
    let token = secret();
    let mut c = s.0.store.0.lock().unwrap();
    let tx = c.transaction()?;
    let (parent,account)=tx.query_row("SELECT p.id,p.account FROM open_codes o JOIN sessions p ON p.id=o.parent JOIN accounts a ON a.id=p.account JOIN gateways g ON g.id=o.gateway WHERE o.hash=?1 AND o.gateway=?2 AND o.expires>?3 AND p.expires>?3 AND a.disabled=0 AND g.account=p.account",params![digest(&input.code),gateway.to_string(),now()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?.ok_or_else(denied)?;
    tx.execute(
        "DELETE FROM open_codes WHERE hash=?1",
        [digest(&input.code)],
    )?;
    tx.execute(
        "INSERT INTO sessions(id,hash,account,gateway,parent,expires) VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            Uuid::new_v4().to_string(),
            digest(&token),
            account,
            gateway.to_string(),
            parent,
            now() + 43200
        ],
    )?;
    tx.commit()?;
    Ok((
        set_cookie("__Host-workbench_session", &token, 43200),
        Json(serde_json::json!({"ok":true})),
    )
        .into_response())
}
