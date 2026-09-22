//! Optional clean-end evidence, separate from the strict v1 commit manifest.
use super::{fs::Directory, journal::Meta};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Lifecycle {
    pub schema_version: u32,
    pub run_epoch: Uuid,
    pub workspace_id: String,
    pub ended_at: String,
    pub final_meta_digest: String,
    pub saved_record_seq: u64,
}
pub(super) fn record(
    dir: &Directory,
    meta: &Meta,
    committed: &[u8],
    mut allowed: impl FnMut() -> bool,
) -> std::io::Result<()> {
    if !meta.ended || !allowed() {
        return Ok(());
    }
    let now = chrono::Utc::now();
    if !chrono::DateTime::parse_from_rfc3339(&meta.started_at).is_ok_and(|start| start <= now) {
        return Ok(());
    }
    let proof = Lifecycle {
        schema_version: 1,
        run_epoch: meta.run_epoch,
        workspace_id: meta.workspace_id.clone(),
        ended_at: now.to_rfc3339(),
        final_meta_digest: blake3::hash(committed).to_hex().to_string(),
        saved_record_seq: meta.saved_record_seq,
    };
    let bytes = serde_json::to_vec(&proof)?;
    if bytes.len() > 4096 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    dir.atomic_when("lifecycle.json", &bytes, &mut allowed)?;
    // A delayed optional fsync must not leave a usable end proof after the
    // shutdown permit was revoked. The writer lock is still held here.
    if !allowed() {
        dir.remove("lifecycle.json")?;
        dir.sync()?;
    }
    Ok(())
}
