//! Read-only presentation of supplied aggregate runtime evidence, not a test
//! runner, Telegram endpoint, repository browser, or workbench control surface.
use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const MAX_REPORT_BYTES: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 600;
const MAX_LIST_ITEMS: usize = 16;
const MAX_COUNT: u32 = 1_000_000;
const CSP: &str =
    "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";
const CASES: [(&str, &str); 7] = [
    ("p1Text", "P1 同一会话文本与来源隔离"),
    ("p2Command", "P2 命令审批"),
    ("p2Files", "P2 已测文件审批"),
    ("p2Questions", "P2 选项与显式 Reply 文字回答"),
    ("p2Queue", "P2 独立排队取消"),
    ("p2FileScope", "P2 扩展文件范围"),
    ("p2Invalidation", "P2 交互失效与故障边界"),
];

#[derive(Parser)]
#[command(about = "Prepare or serve a bounded, read-only runtime evidence report")]
struct Args {
    /// Existing aggregate JSON; never raw logs, credentials, or session data.
    #[arg(long)]
    report: PathBuf,
    /// Write one new private HTML file and exit without opening any listener.
    #[arg(long, conflicts_with_all = ["listen", "port"])]
    output_html: Option<PathBuf>,
    /// Serve address; defaults to loopback 127.0.0.1. No listener in offline mode.
    #[arg(long)]
    listen: Option<Ipv4Addr>,
    /// Serve port; defaults to zero, allowing the OS to select an unused port.
    #[arg(long)]
    port: Option<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Report {
    schema_version: u8,
    kind: String,
    tested_product_commit: String,
    cli_version: String,
    cases: Vec<Case>,
    regular_checks: RegularChecks,
    limitations: Vec<String>,
    user_checklist: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    id: String,
    status: EvidenceStatus,
    detail: String,
    artifact_id: Option<String>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum EvidenceStatus {
    Passed,
    Failed,
    Pending,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegularChecks {
    artifact_id: String,
    rust: Counts,
    web: Counts,
    tools: Counts,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    passed: u32,
    failed: u32,
    ignored: u32,
}

fn safe_text(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_TEXT_BYTES
        && !value.chars().any(|character| {
            character.is_control()
                || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}
fn validate_artifact(value: &str) -> Result<()> {
    let id = Uuid::parse_str(value).context("invalid managed artifact UUID")?;
    ensure!(
        id.get_version_num() == 4
            && id.get_variant() == uuid::Variant::RFC4122
            && id.hyphenated().to_string() == value,
        "managed artifact must be a canonical lowercase v4 UUID"
    );
    Ok(())
}
fn validate_list(items: &[String]) -> Result<()> {
    ensure!(
        (1..=MAX_LIST_ITEMS).contains(&items.len()) && items.iter().all(|item| safe_text(item)),
        "invalid bounded report list"
    );
    Ok(())
}
fn validate(report: &Report) -> Result<()> {
    ensure!(report.schema_version == 1, "unsupported report schema");
    ensure!(report.kind == "runtime-evidence", "unexpected report kind");
    ensure!(
        report.tested_product_commit.len() == 40
            && report
                .tested_product_commit
                .bytes()
                .all(|byte| { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) }),
        "invalid tested product commit"
    );
    ensure!(
        !report.cli_version.trim().is_empty()
            && report.cli_version.len() <= 80
            && report.cli_version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'.' | b'-' | b'_')
            }),
        "invalid CLI version"
    );
    ensure!(
        report.cases.len() == CASES.len(),
        "unexpected evidence case count"
    );
    for (id, _) in CASES {
        ensure!(
            report.cases.iter().filter(|case| case.id == id).count() == 1,
            "missing or duplicate evidence case"
        );
    }
    for case in &report.cases {
        ensure!(safe_text(&case.detail), "invalid evidence detail");
        match (case.status, case.artifact_id.as_deref()) {
            (EvidenceStatus::Pending, None) => {}
            (EvidenceStatus::Passed | EvidenceStatus::Failed, Some(id)) => validate_artifact(id)?,
            _ => anyhow::bail!("case status and artifact do not match"),
        }
    }
    validate_artifact(&report.regular_checks.artifact_id)?;
    for counts in [
        report.regular_checks.rust,
        report.regular_checks.web,
        report.regular_checks.tools,
    ] {
        ensure!(
            counts.passed <= MAX_COUNT && counts.failed <= MAX_COUNT && counts.ignored <= MAX_COUNT,
            "check counts exceed limit"
        );
    }
    validate_list(&report.limitations)?;
    validate_list(&report.user_checklist)
}

fn read_report(path: &Path) -> Result<Report> {
    let file = std::fs::File::open(path).context("open aggregate report")?;
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read aggregate report")?;
    ensure!(bytes.len() <= MAX_REPORT_BYTES, "report exceeds 16 KiB");
    let report: Report = serde_json::from_slice(&bytes).context("parse aggregate report")?;
    validate(&report)?;
    Ok(report)
}
fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
fn list(html: &mut String, heading: &str, items: &[String], ordered: bool) {
    html.push_str("<section><h2>");
    html.push_str(heading);
    html.push_str(if ordered { "</h2><ol>" } else { "</h2><ul>" });
    for item in items {
        html.push_str("<li>");
        html.push_str(&escape_html(item));
        html.push_str("</li>");
    }
    html.push_str(if ordered {
        "</ol></section>"
    } else {
        "</ul></section>"
    });
}
fn page(report: &Report) -> String {
    let mut html = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"{}\"><title>V3 本地运行证据与待验清单</title><style>body{{font:16px/1.6 system-ui,sans-serif;max-width:800px;margin:2rem auto;padding:0 1rem;color:#17202a;background:#f8fafc}}h1{{line-height:1.3}}section{{background:white;border:1px solid #d7dee7;border-radius:12px;padding:1rem 1.2rem;margin:1rem 0}}li{{margin:.7rem 0}}code,p,li{{overflow-wrap:anywhere}}.passed{{color:#166534}}.failed{{color:#b91c1c}}.pending{{color:#854d0e}}table{{width:100%;border-collapse:collapse}}th,td{{text-align:left;padding:.3rem;border-bottom:1px solid #d7dee7}}</style></head><body><h1>V3 本地运行证据与待验清单</h1><p><strong>Proposed；P2 尚未整体验收，P3 待用户验收。</strong></p><p>此页展示提供的本地合成汇总证据，不运行或复跑测试。已通过的用例不代表 P2 整体验收通过，也不证明真实 Telegram、真实模型或用户跨设备操作已通过。</p><section><h2>证据身份</h2><p>受测产品提交：<code>{}</code></p><p>安装版 CLI：<code>{}</code></p><p>产品提交标识本项目受测版本，不是研究源码提交，也不能证明安装版 CLI 的构建来源。</p></section><section><h2>逐项证据</h2><ul>",
        escape_html(CSP),
        escape_html(&report.tested_product_commit),
        escape_html(&report.cli_version)
    );
    for (id, label) in CASES {
        let case = report
            .cases
            .iter()
            .find(|case| case.id == id)
            .expect("validated evidence ID");
        let (class, status) = match case.status {
            EvidenceStatus::Passed => ("passed", "本地检查通过"),
            EvidenceStatus::Failed => ("failed", "本地检查失败"),
            EvidenceStatus::Pending => ("pending", "待验证"),
        };
        html.push_str(&format!(
            "<li><strong class=\"{class}\">{label} · {status}</strong><p>{}</p>",
            escape_html(&case.detail)
        ));
        if let Some(artifact) = &case.artifact_id {
            html.push_str(&format!(
                "<p>受管证据编号：<code>{}</code></p>",
                escape_html(artifact)
            ));
        }
        html.push_str("</li>");
    }
    html.push_str("</ul></section><section><h2>常规检查计数</h2><p>只呈现提供的计数；ignored 未计作通过，单列专项与常规检查不重复加总。</p><p>受管证据编号：<code>");
    html.push_str(&escape_html(&report.regular_checks.artifact_id));
    html.push_str("</code></p><table><thead><tr><th>范围</th><th>passed</th><th>failed</th><th>ignored</th></tr></thead><tbody>");
    for (label, counts) in [
        ("Rust", report.regular_checks.rust),
        ("Web", report.regular_checks.web),
        ("工具", report.regular_checks.tools),
    ] {
        html.push_str(&format!(
            "<tr><th>{label}</th><td>{}</td><td>{}</td><td>{}</td></tr>",
            counts.passed, counts.failed, counts.ignored
        ));
    }
    html.push_str("</tbody></table></section>");
    list(
        &mut html,
        "限制与尚未证明的范围",
        &report.limitations,
        false,
    );
    list(
        &mut html,
        "待用户执行的验收清单（不视为已通过）",
        &report.user_checklist,
        true,
    );
    html.push_str("<p>本页没有控制或凭证输入接口；请勿将 token、私人会话、问题或命令正文加入报告。</p></body></html>");
    html
}

fn write_html(path: &Path, html: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("create new HTML output")?;
    file.write_all(html.as_bytes())
        .context("write HTML output")?;
    file.sync_all().context("sync HTML output")
}
fn content(content_type: &'static str, body: String) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}
async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_SECURITY_POLICY, CSP),
    ] {
        response
            .headers_mut()
            .insert(name, value.parse().expect("static response header"));
    }
    response
}
async fn index(State(report): State<Arc<Report>>) -> Response {
    content("text/html; charset=utf-8", page(&report))
}
async fn api_report(State(report): State<Arc<Report>>) -> Response {
    content(
        "application/json; charset=utf-8",
        serde_json::to_string(report.as_ref()).expect("validated aggregate report"),
    )
}
fn router(report: Report) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/report", get(api_report))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(security_headers))
        .with_state(Arc::new(report))
}
fn validate_listen(address: Ipv4Addr) -> Result<()> {
    let octets = address.octets();
    ensure!(
        address.is_loopback() || octets[0] == 100 && (64..=127).contains(&octets[1]),
        "listen address must be loopback or an explicitly selected CGNAT IPv4 address"
    );
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let report = read_report(&args.report)?;
    if let Some(path) = args.output_html {
        write_html(&path, &page(&report))?;
        println!("Prepared private runtime evidence HTML; no listener opened.");
        return Ok(());
    }
    let address = args.listen.unwrap_or(Ipv4Addr::LOCALHOST);
    validate_listen(address)?;
    let listener = tokio::net::TcpListener::bind((address, args.port.unwrap_or(0)))
        .await
        .context("bind read-only evidence listener")?;
    println!(
        "V3 read-only runtime evidence: http://{}/",
        listener.local_addr()?
    );
    // Axum also provides automatic read-only HEAD for these two GET routes.
    axum::serve(listener, router(report))
        .await
        .context("serve runtime evidence")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt;

    const ARTIFACT: &str = "00000000-0000-4000-8000-000000000001";
    fn report() -> Report {
        Report {
            schema_version: 1,
            kind: "runtime-evidence".into(),
            tested_product_commit: "a".repeat(40),
            cli_version: "codex-cli 0.156.1".into(),
            cases: CASES
                .iter()
                .enumerate()
                .map(|(index, (id, _))| Case {
                    id: (*id).into(),
                    status: if index < 5 {
                        EvidenceStatus::Passed
                    } else {
                        EvidenceStatus::Pending
                    },
                    detail: "Synthetic aggregate evidence only".into(),
                    artifact_id: (index < 5).then(|| ARTIFACT.into()),
                })
                .collect(),
            regular_checks: RegularChecks {
                artifact_id: ARTIFACT.into(),
                rust: Counts {
                    passed: 7,
                    failed: 0,
                    ignored: 2,
                },
                web: Counts {
                    passed: 3,
                    failed: 0,
                    ignored: 0,
                },
                tools: Counts {
                    passed: 2,
                    failed: 0,
                    ignored: 0,
                },
            },
            limitations: vec!["Real Telegram and P3 remain unverified".into()],
            user_checklist: vec!["Check the current prompt in your authorized private chat".into()],
        }
    }
    #[test]
    fn strict_schema_rejects_unknown_and_duplicate_fields_and_invalid_status() {
        let encoded = serde_json::to_string(&report()).unwrap();
        for mutated in [
            encoded.replacen("{", "{\"unknown\":true,", 1),
            encoded.replacen("{", "{\"schemaVersion\":1,", 1),
            encoded.replacen("\"id\":", "\"id\":\"p1Text\",\"id\":", 1),
            encoded.replacen("\"passed\":7", "\"passed\":7,\"passed\":7", 1),
            encoded.replacen("\"status\":\"passed\"", "\"status\":\"accepted\"", 1),
        ] {
            assert!(serde_json::from_str::<Report>(&mutated).is_err());
        }
        let mut value = serde_json::to_value(report()).unwrap();
        value["cases"][0]["body"] = serde_json::json!("unapproved field");
        assert!(serde_json::from_value::<Report>(value).is_err());
    }
    #[test]
    fn all_fixed_cases_are_required_exactly_once() {
        assert!(validate(&report()).is_ok());
        let mut missing = report();
        missing.cases.pop();
        assert!(validate(&missing).is_err());
        let mut duplicate = report();
        duplicate.cases[6].id = "p1Text".into();
        assert!(validate(&duplicate).is_err());
        let mut unknown = report();
        unknown.cases[6].id = "realTelegramAccepted".into();
        assert!(validate(&unknown).is_err());
    }
    #[test]
    fn identities_artifact_versions_and_status_evidence_pairs_are_strict() {
        for id in [
            "",
            "00000000-0000-0000-0000-000000000000",
            "00000000000040008000000000000001",
            "AAAAAAAA-0000-4000-8000-000000000001",
            "00000000-0000-5000-8000-000000000001",
            "00000000-0000-4000-0000-000000000001",
        ] {
            let mut value = report();
            value.cases[0].artifact_id = Some(id.into());
            assert!(validate(&value).is_err());
        }
        let mut value = report();
        value.cases[0].artifact_id = None;
        assert!(validate(&value).is_err());
        let mut value = report();
        value.cases[6].artifact_id = Some(ARTIFACT.into());
        assert!(validate(&value).is_err());
        for commit in ["a".repeat(39), "A".repeat(40), "g".repeat(40)] {
            let mut value = report();
            value.tested_product_commit = commit;
            assert!(validate(&value).is_err());
        }
        let mut value = report();
        value.kind = "native-attachment-boundaries".into();
        assert!(validate(&value).is_err());
        let mut value = report();
        value.schema_version = 2;
        assert!(validate(&value).is_err());
    }
    #[test]
    fn control_bidi_empty_and_oversized_text_are_rejected_in_every_free_text_list() {
        for text in [
            "".into(),
            " ".into(),
            "bad\ntext".into(),
            "bad\u{1b}text".into(),
            "bad\u{202e}text".into(),
            "bad\u{2066}text".into(),
            "x".repeat(MAX_TEXT_BYTES + 1),
        ] {
            let mut value = report();
            value.cases[0].detail = text.clone();
            assert!(validate(&value).is_err());
            let mut value = report();
            value.limitations[0] = text.clone();
            assert!(validate(&value).is_err());
            let mut value = report();
            value.user_checklist[0] = text;
            assert!(validate(&value).is_err());
        }
        let mut value = report();
        value.limitations.clear();
        assert!(validate(&value).is_err());
        let mut value = report();
        value.user_checklist = vec!["bounded".into(); MAX_LIST_ITEMS + 1];
        assert!(validate(&value).is_err());
        let mut value = report();
        value.regular_checks.rust.ignored = MAX_COUNT + 1;
        assert!(validate(&value).is_err());
    }
    #[test]
    fn all_free_text_is_escaped_and_pending_acceptance_scope_always_visible() {
        let mut value = report();
        let attack = "<script>alert('x') & \"quoted\"</script>";
        value.cases[0].detail = attack.into();
        value.limitations[0] = attack.into();
        value.user_checklist[0] = attack.into();
        validate(&value).unwrap();
        let html = page(&value);
        assert!(!html.contains("<script>"));
        assert_eq!(html.matches(&escape_html(attack)).count(), 3);
        assert!(html.contains("Proposed；P2 尚未整体验收，P3 待用户验收。"));
        assert!(html.contains("不运行或复跑测试"));
        assert!(html.contains("ignored 未计作通过"));
        assert!(html.contains("待用户执行的验收清单（不视为已通过）"));
        assert!(!html.contains("<a "));
        assert!(!html.contains("<script"));
    }
    #[test]
    fn bounded_read_refuses_oversize_and_accepts_valid_report() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        std::fs::write(&path, serde_json::to_vec(&report()).unwrap()).unwrap();
        assert!(read_report(&path).is_ok());
        std::fs::write(&path, vec![b' '; MAX_REPORT_BYTES + 1]).unwrap();
        assert!(read_report(&path).is_err());
    }
    #[test]
    fn offline_flags_conflict_with_any_explicit_listen_or_port() {
        for extra in [["--listen", "127.0.0.1"], ["--port", "0"]] {
            assert!(
                Args::try_parse_from([
                    "report",
                    "--report",
                    "input.json",
                    "--output-html",
                    "output.html",
                    extra[0],
                    extra[1]
                ])
                .is_err()
            );
        }
        let args = Args::try_parse_from([
            "report",
            "--report",
            "input.json",
            "--output-html",
            "output.html",
        ])
        .unwrap();
        assert!(args.output_html.is_some() && args.listen.is_none() && args.port.is_none());
        assert!(Args::try_parse_from(["report", "--report", "input.json"]).is_ok());
        assert!(Args::try_parse_from(["report", "--output-html", "output.html"]).is_err());
    }
    #[test]
    fn html_output_is_private_create_new_and_never_overwrites_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.html");
        let html = page(&report());
        write_html(&path, &html).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), html);
        assert!(write_html(&path, "replacement").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), html);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn html_output_refuses_existing_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "preserved").unwrap();
        let path = directory.path().join("output.html");
        symlink(&target, &path).unwrap();
        assert!(write_html(&path, "replacement").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserved");
        let missing = directory.path().join("missing");
        let dangling = directory.path().join("dangling.html");
        symlink(&missing, &dangling).unwrap();
        assert!(write_html(&dangling, "replacement").is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn serving_address_is_limited_to_loopback_or_explicit_cgnat() {
        for address in ["127.0.0.1", "127.0.0.2", "100.64.0.1", "100.127.255.254"] {
            assert!(validate_listen(address.parse().unwrap()).is_ok());
        }
        for address in [
            "0.0.0.0",
            "192.168.1.2",
            "8.8.8.8",
            "100.63.255.254",
            "100.128.0.1",
        ] {
            assert!(validate_listen(address.parse().unwrap()).is_err());
        }
    }
    #[tokio::test]
    async fn http_is_read_only_has_no_static_files_and_applies_security_headers() {
        let app = router(report());
        for (method, path, status) in [
            ("GET", "/", StatusCode::OK),
            ("GET", "/api/report", StatusCode::OK),
            ("HEAD", "/", StatusCode::OK),
            ("HEAD", "/api/report", StatusCode::OK),
            ("POST", "/", StatusCode::METHOD_NOT_ALLOWED),
            ("PUT", "/api/report", StatusCode::METHOD_NOT_ALLOWED),
            ("DELETE", "/api/report", StatusCode::METHOD_NOT_ALLOWED),
            ("GET", "/report.json", StatusCode::NOT_FOUND),
            ("GET", "/api/control", StatusCode::NOT_FOUND),
            ("GET", "/../Cargo.toml", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], CSP);
            let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
            if method == "HEAD" {
                assert!(body.is_empty());
            } else if path == "/api/report" && method == "GET" {
                validate(&serde_json::from_slice::<Report>(&body).unwrap()).unwrap();
            }
        }
    }
}
