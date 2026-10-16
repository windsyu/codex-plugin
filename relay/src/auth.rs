use crate::{
    App, Failure, Result, bad, denied,
    store::{digest, now, secret},
};
use argon2::{
    Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version,
    password_hash::{PasswordHash, SaltString},
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

#[cfg(test)]
pub(crate) struct LoginGate {
    pub entered: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}
#[cfg(test)]
pub(crate) static LOGIN_GATE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<LoginGate>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Login {
    username: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Change {
    old_password: String,
    password: String,
}
pub(crate) fn valid_password(p: &str) -> bool {
    let n = p.chars().count();
    let common = p
        .to_lowercase()
        .replace('0', "o")
        .replace('1', "i")
        .replace('3', "e")
        .replace(['4', '@'], "a")
        .replace(['5', '$'], "s")
        .replace('7', "t");
    let forbidden = ["password", "qwerty", "letmein", "welcome", "administrator"];
    !forbidden.iter().any(|v| common.contains(v))
        && (15..=128).contains(&n)
        && p.len() <= 512
        && !p.chars().any(char::is_control)
        && ![
            "passwordpassword",
            "password123456789",
            "123456789012345",
            "1234567890123456",
            "qwertyuiopasdfgh",
            "letmeinletmeinletmein",
            "correct horse battery staple",
        ]
        .contains(&p.to_lowercase().as_str())
        && !p.chars().all(|c| c == p.chars().next().unwrap())
}
fn argon() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(65536, 3, 1, None).expect("fixed argon parameters"),
    )
}
pub(crate) fn valid_account_password(username: &str, p: &str) -> bool {
    valid_password(p) && !p.to_lowercase().contains(&username.to_lowercase())
}
pub(crate) fn hash_password_sync(p: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))?;
    argon()
        .hash_password(p.as_bytes(), &salt)
        .map(|v| v.to_string())
        .map_err(|_| anyhow::anyhow!("password hashing failed"))
}
pub(crate) async fn hash(s: &App, p: String) -> Result<String> {
    let permit =
        s.0.passwords
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password_sync(&p).map_err(|_| bad())
    })
    .await
    .map_err(|_| bad())?
}
async fn verify(s: &App, p: String, encoded: Option<String>) -> Result<bool> {
    if p.len() > 512 {
        return Ok(false);
    }
    let permit =
        s.0.passwords
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if let Some(encoded) = encoded {
            PasswordHash::new(&encoded)
                .is_ok_and(|h| argon().verify_password(p.as_bytes(), &h).is_ok())
        } else {
            let salt = SaltString::encode_b64(b"relay-dummy-salt").unwrap();
            let _ = argon().hash_password(p.as_bytes(), &salt);
            false
        }
    })
    .await
    .map_err(|_| bad())
}
pub(crate) fn rate(
    s: &App,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    h: &HeaderMap,
    account: &str,
) -> Result<()> {
    let ip = s.client_ip(h, ip.map(|v| v.0.0))?;
    s.rate(format!("ip:{ip}"), 30)?;
    s.rate(format!("account:{}", digest(account)), 10)
}

pub(crate) fn set_cookie(name: &str, token: &str, age: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "set-cookie",
        format!("{name}={token}; Path=/; Max-Age={age}; Secure; HttpOnly; SameSite=Strict")
            .parse()
            .unwrap(),
    );
    h
}
pub(crate) async fn login(
    State(s): State<App>,
    h: HeaderMap,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<Login>,
) -> Result<Response> {
    rate(&s, ip, &h, &input.username)?;
    if input.username.len() > 64 {
        return Err(denied());
    }
    // A valid original browser parent is consumed exactly once by a successful login.
    // Capturing it before hashing makes concurrent login/logout a visible CAS failure.
    let previous_session =
        crate::cookie(&h, "__Host-relay_session").and_then(|token| s.0.store.session(&token, None));
    let row =
        s.0.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT id,password,auth_generation FROM accounts WHERE username=?1 AND disabled=0",
                [&input.username],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
    if !verify(
        &s,
        input.password,
        row.as_ref().and_then(|(_, h, _)| h.clone()),
    )
    .await?
    {
        return Err(denied());
    }
    let (account, verified_hash, verified_generation) = row.ok_or_else(denied)?;
    #[cfg(test)]
    {
        let gate = LOGIN_GATE.lock().unwrap().get(&input.username).cloned();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }
    let token = secret();
    {
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        // Match the verified generation: disable followed by enable still revokes this login.
        let changed=tx.execute("INSERT INTO sessions(id,hash,account,expires,auth_generation) SELECT ?1,?2,id,?3,auth_generation FROM accounts WHERE id=?4 AND disabled=0 AND password=?5 AND auth_generation=?6",params![Uuid::new_v4().to_string(),digest(&token),now()+43200,account,verified_hash,verified_generation])?;
        if changed != 1 {
            return Err(denied());
        }
        // Do not let a second in-flight login create a replacement after another request
        // consumed its parent. Never revoke on failure or affect other browsers.
        if let Some(previous) = previous_session {
            let replaced=tx.execute("DELETE FROM sessions WHERE id=?1 AND account=?2 AND auth_generation=?3 AND gateway IS NULL AND parent IS NULL AND expires>?4 AND EXISTS(SELECT 1 FROM accounts a WHERE a.id=sessions.account AND a.auth_generation=sessions.auth_generation AND a.disabled=0)",params![previous.id.to_string(),previous.account.to_string(),previous.auth_generation,now()])?;
            if replaced != 1 {
                return Err(denied());
            }
        }
        crate::store::audit(&tx, "login", Some(&account), None)?;
        tx.commit()?;
    }
    s.revoke_invalid();
    Ok((
        set_cookie("__Host-relay_session", &token, 43200),
        Json(serde_json::json!({"ok":true})),
    )
        .into_response())
}
pub(crate) async fn change_password(
    State(s): State<App>,
    h: HeaderMap,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<Change>,
) -> Result<Response> {
    let session = s.portal_session(&h)?;
    rate(&s, ip, &h, &session.account.to_string())?;
    if !valid_password(&input.password) {
        return Err(bad());
    }
    let old = s.0.store.0.lock().unwrap().query_row(
        "SELECT password FROM accounts WHERE id=?1",
        [session.account.to_string()],
        |r| r.get::<_, String>(0),
    )?;
    if input.old_password == input.password {
        return Err(bad());
    }
    if !verify(&s, input.old_password, Some(old.clone())).await? {
        return Err(denied());
    }
    let username = s.0.store.0.lock().unwrap().query_row(
        "SELECT username FROM accounts WHERE id=?1",
        [session.account.to_string()],
        |r| r.get::<_, String>(0),
    )?;
    if input
        .password
        .to_lowercase()
        .contains(&username.to_lowercase())
    {
        return Err(bad());
    }
    let password = hash(&s, input.password).await?;
    {
        let mut c = s.0.store.0.lock().unwrap();
        let tx = c.transaction()?;
        let n = tx.execute(
            "UPDATE accounts SET password=?1,must_change_password=0 WHERE id=?2 AND password=?3 AND disabled=0 AND auth_generation=?4 AND EXISTS(SELECT 1 FROM sessions WHERE id=?5 AND account=?2 AND expires>?6 AND auth_generation=?4)",
            params![password, session.account.to_string(), old,session.auth_generation,session.id.to_string(),now()],
        )?;
        if n != 1 {
            return Err(denied());
        }
        crate::store::revoke_account(&tx, &session.account.to_string())?;
        crate::store::audit(
            &tx,
            "password_changed",
            Some(&session.account.to_string()),
            None,
        )?;
        tx.commit()?;
    }
    s.revoke_invalid();
    Ok((
        set_cookie("__Host-relay_session", "", 0),
        Json(serde_json::json!({"ok":true})),
    )
        .into_response())
}
pub(crate) async fn account(State(s): State<App>, h: HeaderMap) -> Result<Json<serde_json::Value>> {
    let session = s.portal_session(&h)?;
    let username = s.0.store.0.lock().unwrap().query_row(
        "SELECT username FROM accounts WHERE id=?1",
        [session.account.to_string()],
        |r| r.get::<_, String>(0),
    )?;
    Ok(Json(
        serde_json::json!({"accountId":session.account,"username":username,"role":session.role,"mustChangePassword":session.must_change_password}),
    ))
}
pub(crate) async fn logout(State(s): State<App>, h: HeaderMap) -> Result<Response> {
    let session = s.portal_session(&h)?;
    s.0.store
        .0
        .lock()
        .unwrap()
        .execute("DELETE FROM sessions WHERE id=?1", [session.id.to_string()])?;
    crate::store::audit(
        &s.0.store.0.lock().unwrap(),
        "logout",
        Some(&session.account.to_string()),
        None,
    )?;
    s.revoke_invalid();
    Ok((
        set_cookie("__Host-relay_session", "", 0),
        Json(serde_json::json!({"ok":true})),
    )
        .into_response())
}
