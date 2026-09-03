use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::path::Path;

/// Stable identifier for one configured App Server endpoint.
pub fn app_server_source_id(path: &Path) -> String {
    URL_SAFE_NO_PAD
        .encode(blake3::hash(format!("app-server\0{}", path.display()).as_bytes()).as_bytes())
}

/// Stable identifier for one durable Codex store.
pub fn stable_source_id(identity: &str) -> String {
    URL_SAFE_NO_PAD
        .encode(blake3::hash(format!("store\0{identity}\0{identity}").as_bytes()).as_bytes())
}

/// Stable Observer thread identity scoped to its originating Codex store.
pub fn thread_key(source_id: &str, thread_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{source_id}\0{thread_id}"))
}
