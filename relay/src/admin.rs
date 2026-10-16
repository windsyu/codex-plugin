use crate::{
    App, Failure, Result, auth, bad, denied,
    store::{ADMIN_USERNAME, Session, audit, now, revoke_account},
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

#[cfg(test)]
pub(crate) static DELETE_GATE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<auth::LoginGate>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateUser {
    username: String,
    temporary_password: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResetPassword {
    temporary_password: String,
}

fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-.".contains(&c))
}

/// Recheck the live role and session in the same transaction as every mutation,
/// particularly after asynchronous password hashing or a concurrent recovery.
fn authorize(c: &Connection, session: &Session) -> Result<()> {
    let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM accounts a JOIN sessions s ON s.account=a.id WHERE a.id=?1 AND a.role='admin' AND a.disabled=0 AND a.must_change_password=0 AND a.auth_generation=?2 AND s.id=?3 AND s.auth_generation=a.auth_generation AND s.gateway IS NULL AND s.expires>?4)",params![session.account.to_string(),session.auth_generation,session.id.to_string(),now()], |r|r.get(0))?;
    if valid { Ok(()) } else { Err(denied()) }
}
fn user(c: &Connection, id: Uuid, actor: &Session) -> Result<String> {
    if id == actor.account {
        return Err(denied());
    }
    c.query_row(
        "SELECT username FROM accounts WHERE id=?1 AND role='user'",
        [id.to_string()],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(denied)
}
fn session(s: &App, h: &HeaderMap) -> Result<Session> {
    let session = s.portal_session(h)?;
    authorize(&s.0.store.0.lock().unwrap(), &session)?;
    Ok(session)
}

pub(crate) async fn list_users(State(s): State<App>, h: HeaderMap) -> Result<Response> {
    let session = session(&s, &h)?;
    let c = s.0.store.0.lock().unwrap();
    authorize(&c, &session)?;
    let mut statement=c.prepare("SELECT id,username,disabled,must_change_password FROM accounts WHERE role='user' ORDER BY username LIMIT 1000")?;
    let users=statement.query_map([], |r| Ok(serde_json::json!({"accountId":r.get::<_,String>(0)?,"username":r.get::<_,String>(1)?,"disabled":r.get::<_,bool>(2)?,"mustChangePassword":r.get::<_,bool>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(Json(serde_json::json!({"users":users})).into_response())
}
pub(crate) async fn create_user(
    State(s): State<App>,
    h: HeaderMap,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<CreateUser>,
) -> Result<Response> {
    let session = session(&s, &h)?;
    auth::rate(&s, ip, &h, &session.account.to_string())?;
    if !valid_username(&input.username)
        || input.username == ADMIN_USERNAME
        || !auth::valid_account_password(&input.username, &input.temporary_password)
    {
        return Err(bad());
    }
    let password = auth::hash(&s, input.temporary_password).await?;
    let id = Uuid::new_v4();
    {
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        authorize(&tx, &session)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE username=?1)",
            [&input.username],
            |r| r.get(0),
        )?;
        if exists {
            return Err(Failure(StatusCode::CONFLICT));
        }
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM accounts WHERE role='user'", [], |r| {
                r.get(0)
            })?;
        if count >= 1000 {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"account_limit","limit":1000})),
            )
                .into_response());
        }
        tx.execute("INSERT INTO accounts(id,username,password,role,must_change_password) VALUES(?1,?2,?3,'user',1)",params![id.to_string(),input.username,password])?;
        audit(&tx, "admin_user_created", Some(&id.to_string()), None)?;
        tx.commit()?;
    }
    Ok(Json(serde_json::json!({"ok":true,"accountId":id})).into_response())
}
pub(crate) async fn reset_password(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<ResetPassword>,
) -> Result<Json<serde_json::Value>> {
    let session = session(&s, &h)?;
    auth::rate(&s, ip, &h, &session.account.to_string())?;
    let username = user(&s.0.store.0.lock().unwrap(), id, &session)?;
    if !auth::valid_account_password(&username, &input.temporary_password) {
        return Err(bad());
    }
    let password = auth::hash(&s, input.temporary_password).await?;
    {
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        authorize(&tx, &session)?;
        user(&tx, id, &session)?;
        tx.execute(
            "UPDATE accounts SET password=?1,must_change_password=1 WHERE id=?2",
            params![password, id.to_string()],
        )?;
        revoke_account(&tx, &id.to_string())?;
        audit(&tx, "admin_password_reset", Some(&id.to_string()), None)?;
        tx.commit()?;
    }
    s.revoke_invalid();
    Ok(Json(serde_json::json!({"ok":true})))
}
fn set_disabled(
    s: &App,
    h: &HeaderMap,
    id: Uuid,
    disabled: bool,
) -> Result<Json<serde_json::Value>> {
    let session = session(s, h)?;
    {
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        authorize(&tx, &session)?;
        user(&tx, id, &session)?;
        tx.execute(
            "UPDATE accounts SET disabled=?1 WHERE id=?2",
            params![disabled, id.to_string()],
        )?;
        if disabled {
            revoke_account(&tx, &id.to_string())?;
        }
        audit(
            &tx,
            if disabled {
                "admin_user_disabled"
            } else {
                "admin_user_enabled"
            },
            Some(&id.to_string()),
            None,
        )?;
        tx.commit()?;
    }
    s.revoke_invalid();
    Ok(Json(serde_json::json!({"ok":true})))
}
pub(crate) async fn disable_user(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    set_disabled(&s, &h, id, true)
}
pub(crate) async fn enable_user(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    set_disabled(&s, &h, id, false)
}

pub(crate) async fn delete_user(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    h: HeaderMap,
) -> Result<StatusCode> {
    let session = session(&s, &h)?;
    #[cfg(test)]
    {
        let gate = DELETE_GATE.lock().unwrap().get(&id).cloned();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
    {
        // Match control admission: a stale handshake cannot install itself
        // between committing the deletion and cancelling existing connections.
        let mut tunnels = s.0.tunnels.lock().unwrap();
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        authorize(&tx, &session)?;
        let role: Option<String> = tx
            .query_row(
                "SELECT role FROM accounts WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let role = role.ok_or(Failure(StatusCode::NOT_FOUND))?;
        if id == session.account || role != "user" {
            return Err(Failure(StatusCode::FORBIDDEN));
        }
        let gateways = {
            let mut q = tx.prepare("SELECT id FROM gateways WHERE account=?1")?;
            q.query_map([id.to_string()], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        revoke_account(&tx, &id.to_string())?;
        tx.execute("DELETE FROM gateways WHERE account=?1", [id.to_string()])?;
        tx.execute("DELETE FROM accounts WHERE id=?1", [id.to_string()])?;
        audit(&tx, "admin_user_deleted", Some(&id.to_string()), None)?;
        tx.commit()?;
        for gateway in gateways {
            if let Ok(gateway) = Uuid::parse_str(&gateway)
                && let Some(connection) = tunnels.remove(&gateway)
            {
                connection.cancel.send_replace(true);
            }
        }
    }
    s.revoke_invalid();
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
#[path = "account_api_tests.rs"]
mod account_api_tests;
