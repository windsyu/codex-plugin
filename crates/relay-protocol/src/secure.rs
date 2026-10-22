//! Public channel metadata and encrypted endpoint protocol. No keys or crypto in Relay.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CHUNK: usize = 16 * 1024;
pub const RECORD: usize = 64 * 1024;
pub const WIRE_RECORD: usize = 96 * 1024;
pub const HANDSHAKE: usize = 16 * 1024;
pub const WINDOW: usize = 256 * 1024;
pub const CHANNEL_QUEUE: usize = 2 * 1024 * 1024;
pub const CONTROL_RESERVE: usize = 64 * 1024;
pub const ENDPOINT_BUDGET: usize = 32 * 1024 * 1024;
pub const RECORD_LIMIT: u64 = 1 << 20;
pub const BYTE_LIMIT: u64 = 1 << 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Scope {
    Application,
    Run { run_id: Uuid },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessContext {
    pub wire_version: u32,
    pub api_version: u32,
    pub account_id: Uuid,
    pub parent_session_id: Uuid,
    pub grant_id: Uuid,
    pub gateway_id: Uuid,
    pub instance_id: Uuid,
    pub connection_epoch: Uuid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBinding {
    #[serde(flatten)]
    pub context: AccessContext,
    pub channel_id: Uuid,
    pub scope: Scope,
}
impl ChannelBinding {
    pub fn valid(&self) -> bool {
        let c = &self.context;
        c.wire_version == crate::VERSION
            && c.api_version == crate::WORKBENCH_API_VERSION
            && [
                c.account_id,
                c.parent_session_id,
                c.grant_id,
                c.gateway_id,
                c.instance_id,
                c.connection_epoch,
                self.channel_id,
            ]
            .iter()
            .all(|id| !id.is_nil())
            && !matches!(self.scope, Scope::Run { run_id } if run_id.is_nil())
    }
    /// Fixed order/length binary encoding; never depends on JSON object ordering.
    pub fn transcript(&self, client: &[u8], desktop: &[u8]) -> Vec<u8> {
        let c = &self.context;
        let mut out = b"codex-relay-e2ee-v3\0".to_vec();
        out.extend(c.wire_version.to_be_bytes());
        out.extend(c.api_version.to_be_bytes());
        for id in [
            c.account_id,
            c.parent_session_id,
            c.grant_id,
            c.gateway_id,
            c.instance_id,
            c.connection_epoch,
            self.channel_id,
        ] {
            out.extend(id.as_bytes());
        }
        match self.scope {
            Scope::Application => out.push(0),
            Scope::Run { run_id } => {
                out.push(1);
                out.extend(run_id.as_bytes());
            }
        }
        out.extend(client);
        out.extend(desktop);
        out
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Handshake {
    ChannelBinding { binding: ChannelBinding },
    ClientHello { public_key: String },
    ServerHello { public_key: String },
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Http,
    Terminal,
}

// Deliberately not Debug: payloads/paths must not accidentally enter diagnostics.
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Frame {
    Finished,
    Open {
        id: u32,
        kind: Kind,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body_length: usize,
        receive_window: usize,
    },
    Accepted {
        id: u32,
        credit: usize,
    },
    Head {
        id: u32,
        status: u16,
        headers: Vec<(String, String)>,
    },
    Chunk {
        id: u32,
        data: String,
    },
    End {
        id: u32,
    },
    TerminalReady {
        id: u32,
    },
    MessageStart {
        id: u32,
        length: usize,
        binary: bool,
    },
    MessageEnd {
        id: u32,
    },
    Window {
        id: u32,
        credit: usize,
    },
    Reset {
        id: u32,
        code: String,
    },
}

/// Exact business scope, excluding documents/assets and all private control APIs.
pub fn request_scope(method: &str, target: &str) -> Option<Scope> {
    if !crate::allowed(method, target) {
        return None;
    }
    let path = target.split('?').next()?;
    let tail = path.strip_prefix("/workbench/v1/")?;
    if let Some(rest) = tail.strip_prefix("runs/") {
        let (run, operation) = rest.split_once('/').unwrap_or((rest, ""));
        let run_id = Uuid::parse_str(run).ok()?;
        if run_id.is_nil() || run_id.to_string() != run {
            return None;
        }
        return Some(if operation.is_empty() {
            Scope::Application
        } else {
            Scope::Run { run_id }
        });
    }
    Some(Scope::Application)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_do_not_turn_application_into_a_run_bypass() {
        let id = Uuid::new_v4();
        assert_eq!(
            request_scope("GET", "/workbench/v1/runs"),
            Some(Scope::Application)
        );
        assert_eq!(
            request_scope("GET", &format!("/workbench/v1/runs/{id}")),
            Some(Scope::Application)
        );
        assert_eq!(
            request_scope("POST", &format!("/workbench/v1/runs/{id}/stop")),
            Some(Scope::Run { run_id: id })
        );
        assert_eq!(request_scope("GET", "/assets/evil.js"), None);
        assert_eq!(
            request_scope("POST", "/workbench/v1/application/relay/disable"),
            None
        );
    }
}
