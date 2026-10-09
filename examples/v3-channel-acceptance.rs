//! Read-only presentation of an isolated V3 protocol probe's aggregate results.
//! This is not a Telegram endpoint or a workbench control surface.

use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{net::Ipv4Addr, path::PathBuf, sync::Arc};

const CHECKS: [&str; 7] = [
    "normalProxyForwarded",
    "noAttachEndpoint",
    "sameExistingThread",
    "localInputExcluded",
    "sameTextDistinctClientIds",
    "nativeTurnCompleted",
    "twoModelResponsesPerTurn",
];

#[derive(Parser)]
#[command(about = "Serve only the safe aggregate output of the V3 channel probe")]
struct Args {
    /// Existing JSON report written by v3-native-channel-probe --report.
    #[arg(long)]
    report: PathBuf,
    /// Bind only loopback or this machine's Tailscale IPv4 address.
    #[arg(long, default_value = "127.0.0.1")]
    listen: Ipv4Addr,
    /// TCP port; zero lets the operating system select an unused port.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Report {
    schema_version: u8,
    cli_version: String,
    source_commit: String,
    checks: Vec<Check>,
    not_tested: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    passed: bool,
    detail: String,
}

fn validate(report: &Report) -> Result<()> {
    ensure!(report.schema_version == 1, "unsupported report schema");
    ensure!(
        !report.cli_version.is_empty()
            && report.cli_version.len() <= 80
            && report
                .cli_version
                .bytes()
                .all(|c| { c.is_ascii_alphanumeric() || matches!(c, b' ' | b'.' | b'-' | b'_') }),
        "invalid CLI version"
    );
    ensure!(
        report.source_commit.len() == 40
            && report.source_commit.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid source commit"
    );
    ensure!(
        report.checks.len() == CHECKS.len(),
        "unexpected check count"
    );
    for name in CHECKS {
        let matching: Vec<&Check> = report.checks.iter().filter(|c| c.name == name).collect();
        ensure!(matching.len() == 1, "missing or duplicate check: {name}");
    }
    for check in &report.checks {
        ensure!(CHECKS.contains(&check.name.as_str()), "unknown check name");
        ensure!(
            !check.detail.is_empty() && check.detail.len() <= 300 && safe_text(&check.detail),
            "invalid check detail"
        );
    }
    ensure!(
        (1..=16).contains(&report.not_tested.len()),
        "invalid not-tested count"
    );
    for item in &report.not_tested {
        ensure!(
            !item.is_empty() && item.len() <= 200 && safe_text(item),
            "invalid not-tested item"
        );
    }
    Ok(())
}

fn safe_text(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
}

fn escape_html(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&#39;"),
            _ => result.push(character),
        }
    }
    result
}

fn page(report: &Report) -> String {
    let mut html = String::from(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>V3 首片验收</title><style>body{font:16px/1.6 system-ui,sans-serif;max-width:760px;margin:3rem auto;padding:0 1rem;color:#17202a;background:#f8fafc}h1{line-height:1.3}section{background:white;border:1px solid #d7dee7;border-radius:12px;padding:1rem 1.4rem;margin:1rem 0}li{margin:.5rem 0}code{overflow-wrap:anywhere}.pass{color:#166534}.fail{color:#b91c1c}</style></head><body><h1>V3 渠道首片验收</h1><p>此页展示隔离探针生成的只读汇总报告，验证原生 CLI 的来源关联与轮次完成证据。Telegram、生产渠道接入和远程交互后端尚未实现。</p><section><h2>受测环境</h2><p>实际 CLI 版本：<code>",
    );
    html.push_str(&escape_html(&report.cli_version));
    html.push_str("</code></p><p>研究参考源码 commit：<code>");
    html.push_str(&escape_html(&report.source_commit));
    html.push_str("</code></p><p>源码 commit 仅标识研究基线，不代表安装版 CLI 的构建来源。</p></section><section><h2>探针检查</h2><ul>");
    for check in &report.checks {
        html.push_str("<li><strong class=\"");
        html.push_str(if check.passed { "pass" } else { "fail" });
        html.push_str("\">");
        html.push_str(if check.passed { "通过" } else { "失败" });
        html.push_str(" · ");
        html.push_str(&escape_html(&check.name));
        html.push_str("</strong>：");
        html.push_str(&escape_html(&check.detail));
        html.push_str("</li>");
    }
    html.push_str("</ul></section><section><h2>尚未验收</h2><ul>");
    for item in &report.not_tested {
        html.push_str("<li>");
        html.push_str(&escape_html(item));
        html.push_str("</li>");
    }
    html.push_str("</ul></section></body></html>");
    html
}

fn response(content_type: &'static str, body: String) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
        .into_response()
}

async fn index(State(report): State<Arc<Report>>) -> Response {
    response("text/html; charset=utf-8", page(&report))
}

async fn api_report(State(report): State<Arc<Report>>) -> Response {
    response(
        "application/json; charset=utf-8",
        serde_json::to_string(&*report).expect("validated report is serializable"),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let octets = args.listen.octets();
    ensure!(
        args.listen.is_loopback() || octets[0] == 100 && (64..=127).contains(&octets[1]),
        "--listen must be loopback or a Tailscale IPv4 address (100.64.0.0/10)"
    );
    let bytes = std::fs::read(&args.report).context("read report")?;
    ensure!(bytes.len() <= 16 * 1024, "report exceeds 16 KiB");
    let report: Report = serde_json::from_slice(&bytes).context("parse report")?;
    validate(&report)?;
    let listener = tokio::net::TcpListener::bind((args.listen, args.port))
        .await
        .context("bind acceptance listener")?;
    let address = listener.local_addr()?;
    println!("V3 read-only acceptance: http://{address}/");
    let app = Router::new()
        .route("/", get(index))
        .route("/api/report", get(api_report))
        .with_state(Arc::new(report));
    axum::serve(listener, app)
        .await
        .context("serve acceptance page")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report {
            schema_version: 1,
            cli_version: "codex-cli 0.156.1".into(),
            source_commit: "2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4".into(),
            checks: CHECKS
                .iter()
                .map(|name| Check {
                    name: (*name).into(),
                    passed: true,
                    detail: "synthetic proof".into(),
                })
                .collect(),
            not_tested: vec!["Telegram integration".into()],
        }
    }

    #[test]
    fn rejects_missing_duplicate_and_unknown_checks() {
        let mut report = report();
        assert!(validate(&report).is_ok());
        report.checks.pop();
        assert!(validate(&report).is_err());
        report.checks.push(report.checks[0].clone());
        assert!(validate(&report).is_err());
        report.checks[6].name = "unknown".into();
        assert!(validate(&report).is_err());
    }

    #[test]
    fn rejects_unknown_json_fields_and_escapes_report_text() {
        let mut value = serde_json::to_value(report()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Report>(value).is_err());
        let mut report = report();
        report.checks[0].detail = "<script>alert('x') & more</script>".into();
        let html = page(&report);
        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;) &amp; more&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }
}
