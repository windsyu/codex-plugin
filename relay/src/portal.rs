use crate::{
    App, Failure, Result, bad, denied,
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

#[cfg(test)]
pub(crate) static APPROVAL_GATE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<crate::auth::LoginGate>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) struct Pending {
    device_hash: String,
    user_code: String,
    name: String,
    expires: i64,
    bound: Option<Bound>,
    resume: Option<(Uuid, String)>,
}
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct ForgetGate {
    pub reached: std::sync::Arc<tokio::sync::Notify>,
    pub resume: std::sync::Arc<tokio::sync::Notify>,
}
pub(crate) async fn index() -> Response {
    Html(include_str!("../static/index.html")).into_response()
}
pub(crate) async fn script() -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        include_str!("../static/portal.js"),
    )
}
pub(crate) async fn style() -> impl IntoResponse {
    (
        [("content-type", "text/css; charset=utf-8")],
        include_str!("../static/portal.css"),
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
    let resume = match (input.resume_gateway_id, input.resume_credential) {
        (None, None) => None,
        (Some(id), Some(credential)) if !id.is_nil() && credential.len() <= 128 => {
            let hash = digest(&credential);
            let valid: bool = s.0.store.0.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM gateways WHERE id=?1 AND credential=?2)",
                params![id.to_string(), hash],
                |r| r.get(0),
            )?;
            if !valid {
                return Err(denied());
            }
            Some((id, hash))
        }
        _ => return Err(bad()),
    };
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
            resume,
        },
    );
    Ok(Json(BindingPending {
        binding_id: id,
        device_code: device,
        user_code: user.clone(),
        verification_url: format!("{}/authorize#bind={user}", s.0.config.portal_origin),
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
    let account = s.authorized_portal_session(&h)?;
    s.rate(format!("approve:{}", account.account), 20)?;
    let b = s.0.bindings.lock().unwrap();
    let p = b
        .values()
        .find(|p| p.user_code == input.user_code && p.expires > now() && p.bound.is_none())
        .ok_or_else(denied)?;
    if let Some((id, hash)) = &p.resume {
        let owned: bool = s.0.store.0.lock().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM gateways WHERE id=?1 AND credential=?2 AND account=?3)",
            params![id.to_string(), hash, account.account.to_string()],
            |r| r.get(0),
        )?;
        if !owned {
            return Err(denied());
        }
    }
    Ok(Json(
        serde_json::json!({"name":p.name,"reauthorization":p.resume.is_some()}),
    ))
}
pub(crate) async fn binding_approve(
    State(s): State<App>,
    h: HeaderMap,
    Json(input): Json<Approval>,
) -> Result<Json<serde_json::Value>> {
    let account = s.authorized_portal_session(&h)?;
    #[cfg(test)]
    {
        let gate = APPROVAL_GATE.lock().unwrap().get(&account.account).cloned();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
    s.rate(format!("approve:{}", account.account), 20)?;
    let mut b = s.0.bindings.lock().unwrap();
    let p = b
        .values_mut()
        .find(|p| p.user_code == input.user_code && p.expires > now() && p.bound.is_none())
        .ok_or_else(denied)?;
    let id = p
        .resume
        .as_ref()
        .map(|(id, _)| *id)
        .unwrap_or_else(Uuid::new_v4);
    let gateway_origin = s.0.config.gateway_origin(id).ok_or_else(bad)?;
    let token = secret();
    let mut c = s.0.store.0.lock().unwrap();
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (username,generation): (String,i64) = tx.query_row("SELECT a.username,a.auth_generation FROM accounts a JOIN sessions s ON s.account=a.id WHERE a.id=?1 AND a.disabled=0 AND a.must_change_password=0 AND s.id=?2 AND s.gateway IS NULL AND s.parent IS NULL AND s.auth_generation=a.auth_generation AND s.expires>?3",params![account.account.to_string(),account.id.to_string(),now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(denied)?;
    let n = if let Some((_, hash)) = &p.resume {
        tx.execute("UPDATE gateways SET name=?1,credential=?2,auth_generation=?3,workbench_api_version=0 WHERE id=?4 AND account=?5 AND credential=?6",params![p.name,digest(&token),generation,id.to_string(),account.account.to_string(),hash])?
    } else {
        tx.execute("INSERT INTO gateways(id,account,name,credential,auth_generation) VALUES(?1,?2,?3,?4,?5)",params![id.to_string(),account.account.to_string(),p.name,digest(&token),generation])?
    };
    if n != 1 {
        return Err(denied());
    }
    if p.resume.is_some() {
        tx.execute("DELETE FROM sessions WHERE gateway=?1", [id.to_string()])?;
        tx.execute("DELETE FROM open_codes WHERE gateway=?1", [id.to_string()])?;
    }
    crate::store::audit(
        &tx,
        "gateway_bound",
        Some(&account.account.to_string()),
        Some(&id.to_string()),
    )?;
    tx.commit()?;
    drop(c);
    p.bound = Some(Bound {
        account_id: account.account,
        gateway_id: id,
        credential: token,
        gateway_origin,
        username,
    });
    s.revoke_invalid();
    Ok(Json(serde_json::json!({"gatewayId":id})))
}
pub(crate) async fn gateways(
    State(s): State<App>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let account = s.authorized_portal_session(&h)?;
    let c = s.0.store.0.lock().unwrap();
    let mut q =
        c.prepare("SELECT id,name,workbench_api_version,auth_generation FROM gateways WHERE account=?1 ORDER BY name,id LIMIT 1000")?;
    let rows = q
        .query_map([account.account.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    drop(c);
    let tunnels = s.0.tunnels.lock().unwrap();
    Ok(Json(
        serde_json::json!({"gateways":rows.into_iter().map(|(id,name,api,generation)|{
            let uuid=Uuid::parse_str(&id).unwrap();
            // Zero is the schema-2 sentinel for a credential whose first Hello
            // has not arrived yet, not evidence of a mismatched API contract.
            let incompatible=api!=0 && api!=codex_relay_protocol::WORKBENCH_API_VERSION;
            let reauth=generation!=account.auth_generation;
            let connected=tunnels.get(&uuid).is_some_and(|c| !*c.cancel.borrow());
            let online=connected && !reauth && !incompatible;
            let status=if reauth {"reauthenticationRequired"}else if incompatible {"versionIncompatible"}else if online {"online"}else if api==0 {"awaitingConnection"}else{"offline"};
            serde_json::json!({"gatewayId":id,"name":name,"online":online,"status":status,"versionIncompatible":incompatible,"reauthorizationRequired":reauth})
        }).collect::<Vec<_>>()}),
    ))
}
pub(crate) async fn forget(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<StatusCode> {
    let account = s.authorized_portal_session(&h)?;
    #[cfg(test)]
    {
        let gate = s.0.forget_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.reached.notify_one();
            gate.resume.notified().await;
        }
    }
    {
        // Match control admission's lock order so a rebind cannot install a new
        // connection between deleting the old identity and removing its tunnel.
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        let authorized: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions p JOIN accounts a ON a.id=p.account WHERE p.id=?1 AND p.account=?2 AND p.gateway IS NULL AND p.parent IS NULL AND p.expires>?3 AND p.auth_generation=a.auth_generation AND p.auth_generation=?4 AND a.disabled=0 AND a.must_change_password=0)",params![account.id.to_string(),account.account.to_string(),now(),account.auth_generation],|r|r.get(0))?;
        if !authorized {
            return Err(denied());
        }
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
            "DELETE FROM gateways WHERE id=?1 AND account=?2 AND credential=?3 AND EXISTS(SELECT 1 FROM accounts a WHERE a.id=gateways.account AND a.auth_generation=gateways.auth_generation AND a.disabled=0 AND a.must_change_password=0)",
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
    req: axum::extract::Request,
) -> Result<Json<serde_json::Value>> {
    let account = s.authorized_portal_session(&h)?;
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct OpenInput {
            ui_api_version: u32,
        }
        if h.get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            != Some("application/json")
        {
            return Err(Failure(StatusCode::UNSUPPORTED_MEDIA_TYPE));
        }
        let body = axum::body::to_bytes(req.into_body(), 8192)
            .await
            .map_err(|_| bad())?;
        let input: OpenInput = serde_json::from_slice(&body).map_err(|_| bad())?;
        if input.ui_api_version != codex_relay_protocol::WORKBENCH_API_VERSION {
            return Err(Failure(StatusCode::CONFLICT));
        }
        let grant = Uuid::new_v4();
        let tunnels = s.0.tunnels.lock().unwrap();
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        let compatible: Option<(u32,i64)> = tx.query_row("SELECT workbench_api_version,auth_generation FROM gateways WHERE id=?1 AND account=?2",params![id.to_string(),account.account.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((api, generation)) = compatible else {
            return Err(Failure(StatusCode::NOT_FOUND));
        };
        if api != input.ui_api_version || generation != account.auth_generation {
            return Err(Failure(StatusCode::CONFLICT));
        }
        let connection = tunnels
            .get(&id)
            .ok_or(Failure(StatusCode::SERVICE_UNAVAILABLE))?;
        if connection.workbench_api_version != input.ui_api_version || *connection.cancel.borrow() {
            return Err(Failure(StatusCode::CONFLICT));
        }
        let n = tx.execute("INSERT INTO sessions(id,hash,account,gateway,parent,expires,auth_generation,ui_api_version) SELECT ?1,?2,p.account,?3,p.id,MIN(p.expires,?4),a.auth_generation,?5 FROM sessions p JOIN accounts a ON a.id=p.account WHERE p.id=?6 AND p.account=?7 AND p.gateway IS NULL AND p.parent IS NULL AND p.expires>?8 AND p.auth_generation=a.auth_generation AND a.disabled=0 AND a.must_change_password=0",params![grant.to_string(),digest(&secret()),id.to_string(),now()+43200,input.ui_api_version,account.id.to_string(),account.account.to_string(),now()])?;
        if n != 1 {
            return Err(denied());
        }
        tx.commit()?;
        Ok(Json(
            serde_json::json!({"url":format!("{}/g/{id}/#grant={grant}&session={}&api={}",s.0.config.portal_origin,account.id,input.ui_api_version)}),
        ))
    }
}
