//! Bounded public relay transport. No model, filesystem or CLI control protocol.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const VERSION: u32 = 3;
/// Central UI, application/start and terminal contract. Strict match required.
pub const WORKBENCH_API_VERSION: u32 = 3;
pub mod secure;
/// Exact document routes shared by the local server and central relay UI.
/// `/relay` is local-only and deliberately absent from remote routing.
pub const APPLICATION_PAGES: &[&str] = &["/", "/history", "/launch", "/settings"];
pub const RUN_PAGES: &[&str] = &[
    "chat", "terminal", "files", "search", "git", "history", "usage", "settings", "access",
];

pub fn document_path(path: &str) -> bool {
    if APPLICATION_PAGES.contains(&path) {
        return true;
    }
    let Some(tail) = path.strip_prefix("/runs/") else {
        return false;
    };
    let Some((id, page)) = tail.split_once('/') else {
        return false;
    };
    Uuid::parse_str(id).is_ok_and(|value| !value.is_nil() && value.to_string() == id)
        && RUN_PAGES.contains(&page)
}
pub const MAX_HTTP_BODY: usize = 2 * 1024 * 1024;
pub const MAX_FRAME: usize = 128 * 1024;
// Native snapshots contain a styled 200x500 checkpoint (22 UTF-8 bytes/cell)
// plus a 1 MiB replay, base64 encoded. Keep this separate from HTTP chunk size.
pub const MAX_WS_MESSAGE: usize = 16 * 1024 * 1024;
pub const MAX_WS_STREAMS: usize = 4;
pub const MAX_STREAMS: usize = 64;
pub const RENEW_SECONDS: u64 = 10;
pub const LEASE_SECONDS: u64 = 30;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hello {
    pub version: u32,
    #[serde(default)]
    pub workbench_api_version: u32,
    pub gateway_id: Uuid,
    pub instance_id: Uuid,
    pub client_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Welcome {
    pub version: u32,
    pub account_id: Uuid,
    pub gateway_id: Uuid,
    pub connection_epoch: Uuid,
    pub gateway_origin: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Http,
    WebSocket,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Open {
    pub stream_id: Uuid,
    pub session_id: Uuid,
    pub instance_id: Uuid,
    pub kind: StreamKind,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToGateway {
    OpenChannel { binding: secure::ChannelBinding },
    Open { request: Open },
    Renew { session_ids: Vec<Uuid> },
    Revoke { session_id: Uuid },
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToRelay {
    ChannelFailed { channel_id: Uuid, code: String },
    Heartbeat,
    StreamFailed { stream_id: Uuid, code: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Data {
    RequestEnd,
    ResponseHead {
        status: u16,
        headers: Vec<(String, String)>,
    },
    ResponseEnd,
    WebSocketReady,
    Error {
        code: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingStart {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_gateway_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_credential: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingPending {
    pub binding_id: Uuid,
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in_seconds: u64,
    pub interval: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingPoll {
    pub device_code: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bound {
    #[serde(default)]
    pub username: String,
    pub account_id: Uuid,
    pub gateway_id: Uuid,
    pub credential: String,
    pub gateway_origin: String,
}

/// Canonical V2 paths only. No application/connect, pairing, V3, model proxy,
/// unknown API or absolute URI can be turned into an owner operation.
pub fn allowed(method: &str, target: &str) -> bool {
    if target.len() > 8192 || !target.starts_with('/') || target.contains('#') {
        return false;
    }
    let path = target.split('?').next().unwrap_or("");
    if path.contains('%') || path.contains("..") || path.contains("//") || path.contains('\\') {
        return false;
    }
    if document_path(path) {
        return method == "GET";
    }
    if path.starts_with("/assets/") {
        return method == "GET" && path.len() > 8;
    }
    let Some(tail) = path.strip_prefix("/workbench/v1/") else {
        return false;
    };
    match tail {
        "application"
        | "library/sources"
        | "library/projects"
        | "library/entries"
        | "library/session-groups" => {
            return method == "GET";
        }
        "application/settings" => return matches!(method, "GET" | "PUT"),
        "application/directories" | "launch-targets" | "library/refresh" | "library/preview" => {
            return method == "POST";
        }
        "runs" => return matches!(method, "GET" | "POST"),
        _ => {}
    }
    let segments: Vec<_> = tail.split('/').collect();
    if segments.len() >= 2 && segments[0] == "launch-operations" {
        return segments.len() == 2 && uuid(segments[1]) && method == "GET";
    }
    if segments.len() >= 3 && segments[0] == "library" && segments[1] == "entries" {
        return match segments.as_slice() {
            [_, _, entry] if !entry.is_empty() => method == "GET",
            [_, _, entry, "details"] if !entry.is_empty() => method == "GET",
            [_, _, entry, "organization"] if !entry.is_empty() => matches!(method, "GET" | "PUT"),
            _ => false,
        };
    }
    if segments.len() == 3 && segments[0] == "library" && segments[1] == "session-groups" {
        return !segments[2].is_empty() && method == "GET";
    }
    if segments.len() < 2 || segments[0] != "runs" || !uuid(segments[1]) {
        return false;
    }
    if segments.len() == 2 {
        return method == "GET";
    }
    let run_tail = segments[2..].join("/");
    match run_tail.as_str() {
        "run"
        | "live/snapshot"
        | "live/events"
        | "terminal"
        | "history"
        | "settings"
        | "access"
        | "access/pairing"
        | "history/usage"
        | "history/cleanup/jobs"
        | "workspace/files"
        | "workspace/file"
        | "workspace/search"
        | "workspace/git/status"
        | "workspace/git/log"
        | "workspace/git/diff" => {
            method == "GET"
                || (run_tail == "settings" && method == "PUT")
                || (run_tail == "history/cleanup/jobs" && method == "POST")
        }
        "stop"
        | "run/stop"
        | "access/enable"
        | "access/disable"
        | "history/usage/refresh"
        | "history/cleanup/preview" => method == "POST",
        _ => {
            let s: Vec<_> = run_tail.split('/').collect();
            match s.as_slice() {
                ["requests", id] => method == "GET" && uuid(id),
                ["access", "devices", id] => method == "DELETE" && uuid(id),
                ["history", epoch] | ["history", epoch, "status"] => method == "GET" && uuid(epoch),
                ["history", epoch, "requests", id] => method == "GET" && uuid(epoch) && uuid(id),
                ["history", "cleanup", "jobs", id] | ["history", "cleanup", "previews", id] => {
                    method == "GET" && uuid(id)
                }
                ["history", "cleanup", "jobs", id, "cancel"] => method == "POST" && uuid(id),
                _ => false,
            }
        }
    }
}
fn uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_groups_and_organization_have_exact_owner_api_routes() {
        for path in [
            "/workbench/v1/library/session-groups?section=primary",
            "/workbench/v1/library/session-groups/g_synthetic?anchorEntryId=e_synthetic&revision=r_synthetic",
            "/workbench/v1/library/entries/e_synthetic/organization",
        ] {
            assert!(allowed("GET", path), "{path}");
            for method in ["POST", "DELETE", "PATCH", "CONNECT"] {
                assert!(!allowed(method, path), "{method} {path}");
            }
            assert_eq!(allowed("PUT", path), path.ends_with("/organization"));
        }
        for path in [
            "/workbench/v1/library/session-groups/",
            "/workbench/v1/library/session-groups/g_synthetic/extra",
            "/workbench/v1/library/session-%67roups",
            "/workbench/v1/library/entries//organization",
            "/workbench/v1/library/entries/e_synthetic/organization/extra",
            "/workbench/v1/library/entries/e_synthetic/%6frganization",
        ] {
            for method in ["GET", "PUT"] {
                assert!(!allowed(method, path), "{method} {path}");
            }
        }
    }
    #[test]
    fn document_routes_are_explicit_canonical_and_get_only() {
        let run = Uuid::new_v4();
        for path in APPLICATION_PAGES
            .iter()
            .map(|p| p.to_string())
            .chain(RUN_PAGES.iter().map(|p| format!("/runs/{run}/{p}")))
        {
            assert!(document_path(&path), "{path}");
            assert!(allowed("GET", &path));
            assert!(!allowed("POST", &path));
        }
        for path in [
            "/relay".to_string(),
            "/history/".to_string(),
            "/login".to_string(),
            "/workbench/v1/unknown".to_string(),
            format!("/runs/{run}/unknown"),
            format!("/runs/{run}/chat/"),
            format!("/runs/{run}/chat/extra"),
            format!("/runs/{}/chat", run.simple()),
            format!("/runs/{}/chat", run.to_string().to_uppercase()),
            format!("/runs/{}/chat", Uuid::nil()),
            format!("/runs/{run}/%63hat"),
            format!("/runs/{run}/../chat"),
        ] {
            assert!(!document_path(&path), "{path}");
            assert!(!allowed("GET", &path), "{path}");
        }
    }
    #[test]
    fn route_boundary() {
        let run = Uuid::new_v4();
        assert!(allowed(
            "GET",
            &format!("/workbench/v1/runs/{run}/terminal?epoch={run}")
        ));
        assert!(allowed("POST", "/workbench/v1/runs"));
        for path in [
            "/v1/control",
            "/v2/send",
            "/workbench/v1/application/connect",
            "/workbench/v1/application/telegram",
            "/workbench/v1/pair",
            "/assets/../secret",
            "/assets/%2e%2e/secret",
            "https://localhost/",
        ] {
            assert!(!allowed("GET", path), "{path}");
        }
        assert!(!allowed("CONNECT", "/"));
    }

    #[test]
    fn directory_browsing_is_explicit_and_local_controls_stay_private() {
        assert!(allowed("POST", "/workbench/v1/application/directories"));
        for method in ["GET", "PUT", "DELETE", "PATCH", "CONNECT"] {
            assert!(!allowed(method, "/workbench/v1/application/directories"));
        }
        for path in [
            "/workbench/v1/application/directories/",
            "/workbench/v1/application/directories/child",
            "/workbench/v1/application/%64irectories",
            "/workbench/v1/application/connect",
            "/workbench/v1/application/pick-directory",
            "/workbench/v1/application/stop",
            "/workbench/v1/application/status",
            "/workbench/v1/application/relay",
            "/workbench/v1/application/relay/bind",
        ] {
            for method in ["GET", "POST", "PUT", "DELETE"] {
                assert!(!allowed(method, path), "{method} {path}");
            }
        }
    }
}
