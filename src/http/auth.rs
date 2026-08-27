use std::net::SocketAddr;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};

const SESSION_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Serialize, Deserialize)]
struct PairPayload {
    kind: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionPayload {
    kind: String,
    nonce: String,
    exp: i64,
}

pub fn generate_pair_code(token: &str) -> Result<String> {
    sign(
        token,
        &PairPayload {
            kind: "pair".into(),
        },
    )
}

pub fn generate_pairing_url(bind: SocketAddr, token: &str) -> Result<String> {
    Ok(format!(
        "http://{bind}/#pair={}",
        generate_pair_code(token)?
    ))
}

pub fn redeem_pair_code(token: &str, code: &str, now: i64) -> Result<String> {
    let payload: PairPayload = verify(token, code)?;
    if payload.kind != "pair" {
        anyhow::bail!("signed value has the wrong purpose");
    }
    let mut nonce = [0_u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    sign(
        token,
        &SessionPayload {
            kind: "session".into(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            exp: now.saturating_add(SESSION_TTL_SECONDS),
        },
    )
}

pub fn verify_session(token: &str, cookie: &str, now: i64) -> bool {
    let Ok(payload) = verify::<SessionPayload>(token, cookie) else {
        return false;
    };
    payload.kind == "session" && payload.exp >= now
}

fn key(token: &str) -> Result<[u8; 32]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .context("decode bearer secret")?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("bearer secret must decode to 32 bytes"))
}

fn sign(token: &str, payload: &impl Serialize) -> Result<String> {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload)?);
    let signature = blake3::keyed_hash(&key(token)?, body.as_bytes());
    Ok(format!(
        "{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.as_bytes())
    ))
}

fn verify<T: for<'de> Deserialize<'de>>(token: &str, signed: &str) -> Result<T> {
    let (body, signature) = signed
        .split_once('.')
        .context("signed value is malformed")?;
    let expected = blake3::keyed_hash(&key(token)?, body.as_bytes());
    let supplied = URL_SAFE_NO_PAD.decode(signature)?;
    if !constant_time_eq(expected.as_bytes(), &supplied) {
        anyhow::bail!("signed value is invalid");
    }
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body)?)?)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_is_reusable_for_one_startup_and_rotation_invalidates_it() -> Result<()> {
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let rotated = URL_SAFE_NO_PAD.encode([8_u8; 32]);
        let pair = generate_pair_code(&token)?;
        let session = redeem_pair_code(&token, &pair, 101)?;
        assert!(verify_session(&token, &session, 102));
        assert!(!verify_session(&rotated, &session, 102));
        let second_session = redeem_pair_code(&token, &pair, 102)?;
        assert!(verify_session(&token, &second_session, 103));
        assert!(redeem_pair_code(&rotated, &pair, 103).is_err());
        assert_ne!(generate_pair_code(&rotated)?, pair);
        Ok(())
    }

    #[test]
    fn pairing_url_is_stable_for_one_startup_and_hides_bearer_secret() -> Result<()> {
        let token = URL_SAFE_NO_PAD.encode([9_u8; 32]);
        let bind = "127.0.0.1:4765".parse()?;
        let url = generate_pairing_url(bind, &token)?;
        assert!(url.starts_with("http://127.0.0.1:4765/#pair="));
        assert!(!url.contains(&token));
        assert_eq!(url, generate_pairing_url(bind, &token)?);

        let code = url.split("#pair=").nth(1).expect("pairing fragment");
        let session = redeem_pair_code(&token, code, 101)?;
        assert!(verify_session(&token, &session, 102));
        Ok(())
    }
}
