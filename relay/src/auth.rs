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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Login {
    username: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Redeem {
    token: String,
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
async fn hash(s: &App, p: String) -> Result<String> {
    let permit =
        s.0.passwords
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure(StatusCode::TOO_MANY_REQUESTS))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| bad())?;
        argon()
            .hash_password(p.as_bytes(), &salt)
            .map(|v| v.to_string())
            .map_err(|_| bad())
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
fn rate(
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
    let row =
        s.0.store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT id,password FROM accounts WHERE username=?1 AND disabled=0",
                [&input.username],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
    if !verify(
        &s,
        input.password,
        row.as_ref().and_then(|(_, h)| h.clone()),
    )
    .await?
    {
        return Err(denied());
    }
    let (account, verified_hash) = row.ok_or_else(denied)?;
    let token = secret();
    let c = s.0.store.0.lock().unwrap();
    // Compare the verified hash atomically with insertion: a concurrent reset/disable wins.
    let changed=c.execute("INSERT INTO sessions(id,hash,account,expires) SELECT ?1,?2,id,?3 FROM accounts WHERE id=?4 AND disabled=0 AND password=?5",params![Uuid::new_v4().to_string(),digest(&token),now()+43200,account,verified_hash])?;
    if changed != 1 {
        return Err(denied());
    }
    crate::store::audit(&c, "login", Some(&account), None)?;
    Ok((
        set_cookie("__Host-relay_session", &token, 43200),
        Json(serde_json::json!({"ok":true})),
    )
        .into_response())
}
pub(crate) async fn redeem(
    State(s): State<App>,
    h: HeaderMap,
    ip: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(input): Json<Redeem>,
) -> Result<Response> {
    rate(&s, ip, &h, &input.token)?;
    if !valid_password(&input.password) || input.token.len() > 128 {
        return Err(bad());
    }
    let token_hash = digest(&input.token);
    let valid=s.0.store.0.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM tokens t JOIN accounts a ON a.id=t.account WHERE t.hash=?1 AND t.expires>?2 AND a.disabled=0)",params![token_hash,now()],|r|r.get::<_,bool>(0))?;
    if !valid {
        return Err(denied());
    }
    let username = s.0.store.0.lock().unwrap().query_row(
        "SELECT a.username FROM tokens t JOIN accounts a ON a.id=t.account WHERE t.hash=?1",
        [&token_hash],
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
        let account=tx.query_row("SELECT t.account FROM tokens t JOIN accounts a ON a.id=t.account WHERE t.hash=?1 AND t.expires>?2 AND a.disabled=0",params![token_hash,now()],|r|r.get::<_,String>(0)).optional()?.ok_or_else(denied)?;
        tx.execute(
            "UPDATE accounts SET password=?1 WHERE id=?2",
            params![password, account],
        )?;
        tx.execute("DELETE FROM tokens WHERE account=?1", [&account])?;
        tx.execute("DELETE FROM sessions WHERE account=?1", [&account])?;
        crate::store::audit(&tx, "password_redeemed", Some(&account), None)?;
        tx.commit()?;
    }
    s.revoke_invalid();
    Ok(Json(serde_json::json!({"ok":true})).into_response())
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
            "UPDATE accounts SET password=?1 WHERE id=?2 AND password=?3 AND disabled=0",
            params![password, session.account.to_string(), old],
        )?;
        if n != 1 {
            return Err(denied());
        }
        tx.execute(
            "DELETE FROM sessions WHERE account=?1",
            [session.account.to_string()],
        )?;
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
        serde_json::json!({"accountId":session.account,"username":username}),
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
