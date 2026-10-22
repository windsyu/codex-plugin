//! Ephemeral endpoint-only keys. Relay never links this module.
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use codex_relay_protocol::secure::{self, ChannelBinding, Frame};
use ring::{aead, agreement, digest, hkdf, rand::SystemRandom};

pub(super) struct Ephemeral {
    key: agreement::EphemeralPrivateKey,
    pub public: String,
}
struct KeyLength;
impl hkdf::KeyType for KeyLength {
    fn len(&self) -> usize {
        32
    }
}
impl Ephemeral {
    pub fn new() -> Result<Self> {
        let key =
            agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new())
                .map_err(|_| anyhow::anyhow!("key_generation"))?;
        let public = STANDARD.encode(
            key.compute_public_key()
                .map_err(|_| anyhow::anyhow!("public_key"))?
                .as_ref(),
        );
        Ok(Self { key, public })
    }
    pub fn agree(
        self,
        binding: &ChannelBinding,
        peer: &str,
        desktop: bool,
    ) -> Result<(Cipher, Cipher)> {
        ensure!(binding.valid(), "channel_binding");
        let remote = STANDARD
            .decode(peer)
            .map_err(|_| anyhow::anyhow!("public_key"))?;
        ensure!(remote.len() == 65 && remote[0] == 4, "public_key");
        let own = STANDARD.decode(&self.public)?;
        let (client, server) = if desktop {
            (&remote, &own)
        } else {
            (&own, &remote)
        };
        let hash = digest::digest(&digest::SHA256, &binding.transcript(client, server));
        let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &remote);
        let (c2d, d2c) = agreement::agree_ephemeral(self.key, &peer, |secret| -> Result<_> {
            let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, hash.as_ref());
            let prk = salt.extract(secret);
            let make = |label: &[u8]| -> Result<Cipher> {
                let mut key = [0u8; 32];
                prk.expand(&[label], KeyLength)
                    .map_err(|_| anyhow::anyhow!("key_derivation"))?
                    .fill(&mut key)
                    .map_err(|_| anyhow::anyhow!("key_derivation"))?;
                let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key)
                    .map_err(|_| anyhow::anyhow!("key_derivation"))?;
                Ok(Cipher {
                    key: aead::LessSafeKey::new(key),
                    hash: hash.as_ref().try_into().unwrap(),
                    next: 0,
                    bytes: 0,
                })
            };
            Ok((make(b"codex-relay-v3/c2d")?, make(b"codex-relay-v3/d2c")?))
        })
        .map_err(|_| anyhow::anyhow!("key_agreement"))??;
        Ok(if desktop { (d2c, c2d) } else { (c2d, d2c) })
    }
}

pub(super) struct Cipher {
    key: aead::LessSafeKey,
    hash: [u8; 32],
    next: u64,
    bytes: u64,
}
impl Cipher {
    fn context(&self) -> ([u8; 12], [u8; 40]) {
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.next.to_be_bytes());
        let mut aad = [0u8; 40];
        aad[..32].copy_from_slice(&self.hash);
        aad[32..].copy_from_slice(&self.next.to_be_bytes());
        (nonce, aad)
    }
    fn advance(&mut self, n: usize) -> Result<()> {
        ensure!(
            n <= secure::RECORD
                && self.next < secure::RECORD_LIMIT
                && self.bytes + (n as u64) <= secure::BYTE_LIMIT,
            "key_limit"
        );
        self.next += 1;
        self.bytes += n as u64;
        Ok(())
    }
    pub fn seal(&mut self, frame: &Frame) -> Result<Vec<u8>> {
        let mut data = serde_json::to_vec(frame)?;
        let (nonce, aad) = self.context();
        let sequence = self.next;
        self.advance(data.len())?;
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut data,
            )
            .map_err(|_| anyhow::anyhow!("encrypt_failed"))?;
        let mut wire = sequence.to_be_bytes().to_vec();
        wire.extend(data);
        Ok(wire)
    }
    pub fn open(&mut self, wire: &[u8]) -> Result<Frame> {
        ensure!(
            wire.len() >= 24 && wire.len() <= secure::RECORD + 24,
            "record_size"
        );
        ensure!(wire[..8] == self.next.to_be_bytes(), "record_sequence");
        let (nonce, aad) = self.context();
        let mut data = wire[8..].to_vec();
        let plain = self
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                &mut data,
            )
            .map_err(|_| anyhow::anyhow!("record_authentication"))?;
        self.advance(plain.len())?;
        serde_json::from_slice(plain).map_err(|_| anyhow::anyhow!("record_format"))
    }
    pub fn finished(&mut self, wire: &[u8]) -> Result<()> {
        ensure!(self.next == 0, "finished_order");
        if !matches!(self.open(wire)?, Frame::Finished) {
            bail!("finished_required");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_relay_protocol::secure::{AccessContext, Scope};
    use uuid::Uuid;
    fn binding() -> ChannelBinding {
        ChannelBinding {
            context: AccessContext {
                wire_version: 3,
                api_version: 3,
                account_id: Uuid::new_v4(),
                parent_session_id: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                instance_id: Uuid::new_v4(),
                connection_epoch: Uuid::new_v4(),
            },
            channel_id: Uuid::new_v4(),
            scope: Scope::Application,
        }
    }
    #[test]
    fn ephemeral_roundtrip_replay_tampering_and_binding() {
        let b = binding();
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let cp = c.public.clone();
        let sp = s.public.clone();
        let (mut ct, mut cr) = c.agree(&b, &sp, false).unwrap();
        let (mut st, mut sr) = s.agree(&b, &cp, true).unwrap();
        let finished = ct.seal(&Frame::Finished).unwrap();
        sr.finished(&finished).unwrap();
        assert!(sr.open(&finished).is_err());
        cr.finished(&st.seal(&Frame::Finished).unwrap()).unwrap();
        let mut wire = ct.seal(&Frame::End { id: 1 }).unwrap();
        wire[10] ^= 1;
        assert!(sr.open(&wire).is_err());
        wire[10] ^= 1;
        assert!(matches!(sr.open(&wire).unwrap(), Frame::End { id: 1 }));
        let c = Ephemeral::new().unwrap();
        assert!(c.agree(&b, &STANDARD.encode([0; 65]), false).is_err());
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let cp = c.public.clone();
        let sp = s.public.clone();
        let (mut ct, _) = c.agree(&b, &sp, false).unwrap();
        let mut other = b.clone();
        other.channel_id = Uuid::new_v4();
        let (_, mut sr) = s.agree(&other, &cp, true).unwrap();
        assert!(sr.finished(&ct.seal(&Frame::Finished).unwrap()).is_err());
    }
    #[test]
    fn limits_do_not_reset_or_wrap() {
        let b = binding();
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let (mut tx, _) = c.agree(&b, &s.public, false).unwrap();
        tx.next = secure::RECORD_LIMIT;
        assert!(tx.seal(&Frame::Finished).is_err());
        tx.next = 1;
        tx.bytes = secure::BYTE_LIMIT;
        assert!(tx.seal(&Frame::Finished).is_err());
    }
    #[test]
    fn every_binding_field_and_direction_is_authenticated() {
        for changed in 0..9 {
            let b = binding();
            let mut other = b.clone();
            match changed {
                0 => other.context.account_id = Uuid::new_v4(),
                1 => other.context.parent_session_id = Uuid::new_v4(),
                2 => other.context.grant_id = Uuid::new_v4(),
                3 => other.context.gateway_id = Uuid::new_v4(),
                4 => other.context.instance_id = Uuid::new_v4(),
                5 => other.context.connection_epoch = Uuid::new_v4(),
                6 => other.channel_id = Uuid::new_v4(),
                7 => {
                    other.scope = Scope::Run {
                        run_id: Uuid::new_v4(),
                    }
                }
                _ => {}
            }
            let c = Ephemeral::new().unwrap();
            let s = Ephemeral::new().unwrap();
            let cp = c.public.clone();
            let sp = s.public.clone();
            let (mut ct, _) = c.agree(&b, &sp, false).unwrap();
            // Swapping roles reverses the ordered public keys and direction key.
            let (_, mut sr) = s.agree(&other, &cp, changed != 8).unwrap();
            assert!(
                sr.finished(&ct.seal(&Frame::Finished).unwrap()).is_err(),
                "binding field {changed}"
            );
        }
        for version in [0, 2, 4] {
            let mut b = binding();
            b.context.wire_version = version;
            assert!(
                Ephemeral::new()
                    .unwrap()
                    .agree(&b, &Ephemeral::new().unwrap().public, false)
                    .is_err()
            );
        }
    }
    #[test]
    fn invalid_points_truncation_out_of_order_and_missing_finished_fail_closed() {
        let b = binding();
        let mut invalid = vec![0; 65];
        invalid[0] = 4;
        for public in [
            String::new(),
            "not-base64".into(),
            STANDARD.encode([2; 33]),
            STANDARD.encode(invalid),
        ] {
            assert!(Ephemeral::new().unwrap().agree(&b, &public, false).is_err());
        }
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let cp = c.public.clone();
        let sp = s.public.clone();
        let (mut ct, _) = c.agree(&b, &sp, false).unwrap();
        let (_, mut sr) = s.agree(&b, &cp, true).unwrap();
        let first = ct.seal(&Frame::Finished).unwrap();
        for n in 0..first.len() {
            assert!(sr.finished(&first[..n]).is_err());
        }
        sr.finished(&first).unwrap();
        let next = ct.seal(&Frame::End { id: 1 }).unwrap();
        let later = ct.seal(&Frame::End { id: 2 }).unwrap();
        assert!(sr.open(&later).is_err());
        assert!(matches!(sr.open(&next).unwrap(), Frame::End { id: 1 }));
        assert!(matches!(sr.open(&later).unwrap(), Frame::End { id: 2 }));
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let cp = c.public.clone();
        let sp = s.public.clone();
        let (mut ct, _) = c.agree(&b, &sp, false).unwrap();
        let (_, mut sr) = s.agree(&b, &cp, true).unwrap();
        assert!(
            sr.finished(&ct.seal(&Frame::End { id: 1 }).unwrap())
                .is_err()
        );
    }
    #[test]
    fn final_permitted_record_never_reuses_a_sequence_and_new_channel_rekeys() {
        let b = binding();
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let cp = c.public.clone();
        let sp = s.public.clone();
        let (mut ct, _) = c.agree(&b, &sp, false).unwrap();
        let (_, mut sr) = s.agree(&b, &cp, true).unwrap();
        ct.next = secure::RECORD_LIMIT - 1;
        sr.next = ct.next;
        let wire = ct.seal(&Frame::End { id: 1 }).unwrap();
        assert!(matches!(sr.open(&wire).unwrap(), Frame::End { id: 1 }));
        assert!(ct.seal(&Frame::End { id: 2 }).is_err());
        assert!(sr.open(&wire).is_err());
        let c = Ephemeral::new().unwrap();
        let s = Ephemeral::new().unwrap();
        let (mut fresh, _) = c.agree(&b, &s.public, false).unwrap();
        let fresh_wire = fresh.seal(&Frame::Finished).unwrap();
        assert_eq!(&fresh_wire[..8], &0u64.to_be_bytes());
        assert!(sr.open(&fresh_wire).is_err());
        let size = serde_json::to_vec(&Frame::End { id: 1 }).unwrap().len() as u64;
        fresh.bytes = secure::BYTE_LIMIT - size;
        assert!(fresh.seal(&Frame::End { id: 1 }).is_ok());
        assert!(fresh.seal(&Frame::End { id: 2 }).is_err());
    }
    #[test]
    fn actual_browser_webcrypto_interoperates_with_ring() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Command, Stdio},
        };
        let isolated = tempfile::tempdir().unwrap();
        let native = isolated.path().join("native");
        std::fs::create_dir(&native).unwrap();
        let mut peer = Command::new("node")
            .args([
                "--experimental-transform-types",
                "tests/fixtures/secure-webcrypto-peer.mjs",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .env("HOME", isolated.path())
            .env("USERPROFILE", isolated.path())
            .env("CODEX_HOME", &native)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node 22.23.2 is a required development dependency");
        let mut output = BufReader::new(peer.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let hello: serde_json::Value = serde_json::from_str(&line).expect("WebCrypto public hello");
        let b = binding();
        let key = Ephemeral::new().unwrap();
        let public = key.public.clone();
        let (mut tx, mut rx) = key
            .agree(&b, hello["publicKey"].as_str().unwrap(), true)
            .unwrap();
        let value = serde_json::json!({"binding":b,"serverKey":public,"finished":STANDARD.encode(tx.seal(&Frame::Finished).unwrap())});
        writeln!(peer.stdin.as_mut().unwrap(), "{value}").unwrap();
        peer.stdin.take();
        line.clear();
        output.read_line(&mut line).unwrap();
        let result: serde_json::Value =
            serde_json::from_str(&line).expect("WebCrypto authenticated response");
        rx.finished(
            &STANDARD
                .decode(result["finished"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            rx.open(&STANDARD.decode(result["end"].as_str().unwrap()).unwrap())
                .unwrap(),
            Frame::End { id: 7 }
        ));
        assert!(peer.wait().unwrap().success());
    }
}
