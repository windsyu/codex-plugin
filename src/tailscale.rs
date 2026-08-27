use std::net::SocketAddr;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeAccess {
    pub authority: String,
    pub origin: String,
    pub viewer_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    self_: SelfNode,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SelfNode {
    #[serde(rename = "DNSName")]
    dns_name: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

pub fn ensure_serve(bind: SocketAddr, https_port: u16) -> Result<ServeAccess> {
    ensure_serve_with(&RealRunner, bind, https_port)
}

trait Runner {
    fn output(&self, args: &[String]) -> Result<String>;
}

struct RealRunner;

impl Runner for RealRunner {
    fn output(&self, args: &[String]) -> Result<String> {
        let output = Command::new("tailscale")
            .args(args)
            .output()
            .with_context(|| format!("run tailscale {}", args.join(" ")))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            bail!("tailscale {} failed: {}", args.join(" "), error.trim());
        }
        String::from_utf8(output.stdout).context("tailscale output was not UTF-8")
    }
}

fn ensure_serve_with(
    runner: &impl Runner,
    bind: SocketAddr,
    https_port: u16,
) -> Result<ServeAccess> {
    let raw_status = runner.output(&["status".into(), "--json".into()])?;
    let status: Status = serde_json::from_str(&raw_status).context("parse tailscale status")?;
    if status.backend_state != "Running" {
        bail!("Tailscale daemon is not running");
    }
    if !status
        .self_
        .capabilities
        .iter()
        .chain(status.capabilities.iter())
        .any(|capability| capability == "https")
    {
        bail!("Tailscale HTTPS is not enabled for this tailnet");
    }

    let dns_name = status.self_.dns_name.trim_end_matches('.');
    if dns_name.is_empty() {
        bail!("Tailscale MagicDNS name is unavailable");
    }
    let authority = if https_port == 443 {
        dns_name.to_string()
    } else {
        format!("{dns_name}:{https_port}")
    };
    let access = ServeAccess {
        authority: authority.clone(),
        origin: format!("https://{authority}"),
        viewer_url: format!("https://{authority}/"),
    };
    let target = format!("http://{bind}");
    let existing = serve_status(runner)?;
    if !route_matches(&existing, &access.authority, https_port, &target)
        && port_is_configured(&existing, https_port)
    {
        bail!("Tailscale HTTPS port {https_port} already has a different Serve configuration");
    }

    runner.output(&[
        "serve".into(),
        "--bg".into(),
        format!("--https={https_port}"),
        "--yes".into(),
        target.clone(),
    ])?;
    let configured = serve_status(runner)?;
    if !route_matches(&configured, &access.authority, https_port, &target) {
        bail!("Tailscale Serve did not expose the expected Observer route");
    }
    let visibility = runner.output(&["serve".into(), "status".into()])?;
    if !visibility.contains("(tailnet only)") {
        bail!("Tailscale route is not confirmed tailnet-only; Funnel is not permitted");
    }
    Ok(access)
}

fn serve_status(runner: &impl Runner) -> Result<Value> {
    let raw = runner.output(&["serve".into(), "status".into(), "--json".into()])?;
    serde_json::from_str(&raw).context("parse tailscale serve status")
}

fn route_matches(status: &Value, authority: &str, port: u16, target: &str) -> bool {
    let serve_authority = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:{port}")
    };
    status
        .pointer(&format!("/TCP/{port}/HTTPS"))
        .and_then(Value::as_bool)
        == Some(true)
        && status
            .pointer(&format!(
                "/Web/{}/Handlers/~1/Proxy",
                escape_pointer(&serve_authority)
            ))
            .and_then(Value::as_str)
            == Some(target)
}

fn port_is_configured(status: &Value, port: u16) -> bool {
    status.pointer(&format!("/TCP/{port}")).is_some()
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    struct FakeRunner {
        responses: Mutex<VecDeque<String>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(responses: &[&str]) -> Self {
            Self {
                responses: Mutex::new(responses.iter().map(|value| (*value).to_string()).collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn output(&self, args: &[String]) -> Result<String> {
            self.calls.lock().expect("calls lock").push(args.to_vec());
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .context("missing fake response")
        }
    }

    const STATUS: &str = r#"{"BackendState":"Running","Self":{"DNSName":"observer.example.ts.net.","Capabilities":["https"]}}"#;
    const EMPTY: &str = "{}";
    const READY: &str = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"observer.example.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:4765"}}}}}"#;

    #[test]
    fn creates_one_root_proxy_and_returns_stable_url() -> Result<()> {
        let runner = FakeRunner::new(&[STATUS, EMPTY, "configured", READY, "(tailnet only)"]);
        let access = ensure_serve_with(&runner, "127.0.0.1:4765".parse()?, 443)?;
        assert_eq!(access.viewer_url, "https://observer.example.ts.net/");
        assert_eq!(
            runner.calls.lock().expect("calls lock")[2],
            vec![
                "serve",
                "--bg",
                "--https=443",
                "--yes",
                "http://127.0.0.1:4765"
            ]
        );
        Ok(())
    }

    #[test]
    fn reasserts_matching_route_as_tailnet_only_and_refuses_conflicts() -> Result<()> {
        let runner = FakeRunner::new(&[
            STATUS,
            READY,
            "configured",
            READY,
            "https://observer.example.ts.net (tailnet only)",
        ]);
        assert!(ensure_serve_with(&runner, "127.0.0.1:4765".parse()?, 443).is_ok());
        assert_eq!(runner.calls.lock().expect("calls lock").len(), 5);

        let conflict = r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"observer.example.ts.net":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:9999"}}}}}"#;
        let runner = FakeRunner::new(&[STATUS, conflict]);
        assert!(ensure_serve_with(&runner, "127.0.0.1:4765".parse()?, 443).is_err());
        assert_eq!(runner.calls.lock().expect("calls lock").len(), 2);
        Ok(())
    }

    #[test]
    fn refuses_route_without_tailnet_only_confirmation() -> Result<()> {
        let runner =
            FakeRunner::new(&[STATUS, EMPTY, "configured", READY, "available on internet"]);
        assert!(ensure_serve_with(&runner, "127.0.0.1:4765".parse()?, 443).is_err());
        Ok(())
    }
}
