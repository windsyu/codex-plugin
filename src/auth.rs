use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::{Deserialize, Serialize};

const PAIR_TTL_SECONDS: i64 = 5 * 60;
const SESSION_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;
const MAX_USED_NONCES: usize = 1024;

#[derive(Debug, Serialize, Deserialize)]
struct SignedPayload {
    kind: String,
    nonce: String,
    exp: i64,
}

#[derive(Default)]
pub struct PairingNonceStore {
    used: Mutex<VecDeque<(String, i64)>>,
}

impl PairingNonceStore {
    pub fn consume(&self, nonce: &str, exp: i64, now: i64) -> bool {
        let mut used = self.used.lock().expect("pairing nonce store poisoned");
        while used.front().is_some_and(|(_, expires)| *expires < now) {
            used.pop_front();
        }
        if used.iter().any(|(used_nonce, _)| used_nonce == nonce) {
            return false;
        }
        while used.len() >= MAX_USED_NONCES {
            used.pop_front();
        }
        used.push_back((nonce.to_string(), exp));
        true
    }
}

pub fn generate_pair_code(token: &str, now: i64) -> Result<String> {
    let mut nonce = [0_u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    sign(
        token,
        &SignedPayload {
            kind: "pair".into(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            exp: now.saturating_add(PAIR_TTL_SECONDS),
        },
    )
}

pub fn redeem_pair_code(
    token: &str,
    code: &str,
    nonces: &PairingNonceStore,
    now: i64,
) -> Result<String> {
    let payload = verify(token, code, "pair", now)?;
    if !nonces.consume(&payload.nonce, payload.exp, now) {
        anyhow::bail!("pairing code was already used");
    }
    let mut nonce = [0_u8; 24];
    rand::rng().fill_bytes(&mut nonce);
    sign(
        token,
        &SignedPayload {
            kind: "session".into(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            exp: now.saturating_add(SESSION_TTL_SECONDS),
        },
    )
}

pub fn verify_session(token: &str, cookie: &str, now: i64) -> bool {
    verify(token, cookie, "session", now).is_ok()
}

fn key(token: &str) -> Result<[u8; 32]> {
    let decoded = URL_SAFE_NO_PAD
        .decode(token)
        .context("decode bearer secret")?;
    decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("bearer secret must decode to 32 bytes"))
}

fn sign(token: &str, payload: &SignedPayload) -> Result<String> {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload)?);
    let signature = blake3::keyed_hash(&key(token)?, body.as_bytes());
    Ok(format!(
        "{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.as_bytes())
    ))
}

fn verify(token: &str, signed: &str, kind: &str, now: i64) -> Result<SignedPayload> {
    let (body, signature) = signed
        .split_once('.')
        .context("signed value is malformed")?;
    let expected = blake3::keyed_hash(&key(token)?, body.as_bytes());
    let supplied = URL_SAFE_NO_PAD.decode(signature)?;
    if !constant_time_eq(expected.as_bytes(), &supplied) {
        anyhow::bail!("signed value is invalid");
    }
    let payload: SignedPayload = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(body)?)?;
    if payload.kind != kind || payload.exp < now {
        anyhow::bail!("signed value is expired or has the wrong purpose");
    }
    Ok(payload)
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
    fn pair_is_single_use_expires_and_sessions_follow_token_rotation() -> Result<()> {
        let token = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let rotated = URL_SAFE_NO_PAD.encode([8_u8; 32]);
        let store = PairingNonceStore::default();
        let pair = generate_pair_code(&token, 100)?;
        let session = redeem_pair_code(&token, &pair, &store, 101)?;
        assert!(verify_session(&token, &session, 102));
        assert!(!verify_session(&rotated, &session, 102));
        assert!(redeem_pair_code(&token, &pair, &store, 102).is_err());
        let expired = generate_pair_code(&token, 100)?;
        assert!(redeem_pair_code(&token, &expired, &store, 401).is_err());
        Ok(())
    }
}
