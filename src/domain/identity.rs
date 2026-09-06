use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
/// Stable identifier for one durable Codex store.
pub fn stable_source_id(identity: &str) -> String {
    URL_SAFE_NO_PAD
        .encode(blake3::hash(format!("store\0{identity}\0{identity}").as_bytes()).as_bytes())
}

/// Stable logical identity for Gateway-owned terminal runtimes backed by one Codex store.
pub fn session_source_id(store_source_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(
        blake3::hash(format!("gateway-owned-session\0{store_source_id}").as_bytes()).as_bytes(),
    )
}

/// Stable Observer thread identity scoped to its originating Codex store.
pub fn thread_key(source_id: &str, thread_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{source_id}\0{thread_id}"))
}
