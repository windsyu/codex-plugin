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
use std::{io::Read, net::Ipv4Addr, path::PathBuf, sync::Arc};

const MAX_REPORT_BYTES: usize = 16 * 1024;

const CHECKS: [&str; 7] = [
    "normalProxyForwarded",
    "noAttachEndpoint",
    "sameExistingThread",
    "localInputExcluded",
    "sameTextDistinctClientIds",
    "nativeTurnCompleted",
    "twoModelResponsesPerTurn",
];
const BOUNDARY_CHECKS: [(&str, &str); 10] = [
    ("commandApprovalAllow", "原生命令审批：允许"),
    (
        "resolvedStaleDenyCannotReverseAllow",
        "已解决的旧拒绝响应不能推翻允许结果",
    ),
    ("commandApprovalDeny", "原生命令审批：拒绝"),
    (
        "resolvedStaleAllowCannotReverseDeny",
        "已解决的旧允许响应不能推翻拒绝结果",
    ),
    ("planQuestionRoundTrip", "原生计划问题的提问与回答"),
    (
        "queueWaitsBehindOtherClientTurn",
        "忙碌轮次后再处理排队消息",
    ),
    ("otherClientSteerWithheld", "其他客户端插话时暂扣最终回复"),
    (
        "lateSteerAfterFinalWithheld",
        "最终内容后仍到达的插话继续暂扣回复",
    ),
    (
        "unreadAckNoReplayAndGapWithheld",
        "故意不读取确认时不自动重发，捕获缺口暂扣输出",
    ),
    (
        "semanticQueuePreservesNativeDraft",
        "语义队列保留原生未发送草稿",
    ),
];
const ATTACHMENT_CHECKS: [(&str, &str); 5] = [
    (
        "originViaTuiSubscriptionWithoutControlResume",
        "现有 TUI 订阅提供来源证据",
    ),
    (
        "approvalViaTuiSubscriptionWithoutControlResume",
        "现有 TUI 订阅提供审批证据",
    ),
    (
        "questionViaTuiSubscriptionWithoutControlResume",
        "现有 TUI 订阅提供提问证据",
    ),
    (
        "captureConsumerLossDoesNotBlockTuiAndInvalidatesProof",
        "观察者断开不阻塞原生任务，并使来源证明失效",
    ),
    (
        "unloadedTurnStartRejectsWithoutColdLoadOrQueue",
        "目标卸载后拒绝提交，不冷加载或排队",
    ),
];

#[derive(Parser)]
#[command(about = "Serve only the safe aggregate output of the V3 channel probe")]
struct Args {
    /// Existing JSON report written by v3-native-channel-probe --report.
    #[arg(long)]
    report: PathBuf,
    /// Optional P0b semantic-boundary report from the isolated probe.
    #[arg(long)]
    boundaries_report: Option<PathBuf>,
    /// Optional P0c loaded-thread attachment report from the isolated probe.
    #[arg(long)]
    attachment_report: Option<PathBuf>,
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BoundariesReport {
    schema_version: u8,
    kind: String,
    cli_version: String,
    source_commit: String,
    checks: Vec<BoundaryCheck>,
    not_tested: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundaryCheck {
    name: String,
    status: BoundaryStatus,
    detail: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum BoundaryStatus {
    Passed,
    Failed,
    Unsupported,
}

#[derive(Clone)]
struct PageState {
    first: Report,
    boundaries: Option<BoundariesReport>,
    attachment: Option<BoundariesReport>,
}

fn validate(report: &Report) -> Result<()> {
    ensure!(report.schema_version == 1, "unsupported report schema");
    validate_identity(&report.cli_version, &report.source_commit)?;
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
    validate_not_tested(&report.not_tested)?;
    Ok(())
}

fn validate_semantic(
    report: &BoundariesReport,
    expected_kind: &str,
    expected_checks: &[(&str, &str)],
) -> Result<()> {
    ensure!(report.schema_version == 1, "unsupported semantic schema");
    ensure!(report.kind == expected_kind, "unexpected semantic kind");
    validate_identity(&report.cli_version, &report.source_commit)?;
    ensure!(
        report.checks.len() == expected_checks.len(),
        "unexpected semantic check count"
    );
    for (name, _) in expected_checks {
        ensure!(
            report.checks.iter().filter(|c| c.name == *name).count() == 1,
            "missing or duplicate semantic check: {name}"
        );
    }
    for check in &report.checks {
        ensure!(
            expected_checks.iter().any(|(name, _)| *name == check.name),
            "unknown semantic check name"
        );
        ensure!(
            !check.detail.is_empty() && check.detail.len() <= 300 && safe_text(&check.detail),
            "invalid semantic check detail"
        );
    }
    validate_not_tested(&report.not_tested)
}

fn validate_identity(cli_version: &str, source_commit: &str) -> Result<()> {
    ensure!(
        !cli_version.is_empty()
            && cli_version.len() <= 80
            && cli_version
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b' ' | b'.' | b'-' | b'_')),
        "invalid CLI version"
    );
    ensure!(
        source_commit.len() == 40 && source_commit.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid source commit"
    );
    Ok(())
}

fn validate_not_tested(items: &[String]) -> Result<()> {
    ensure!((1..=16).contains(&items.len()), "invalid not-tested count");
    for item in items {
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

fn read_report(path: &PathBuf, label: &'static str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {label}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_REPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label}"))?;
    ensure!(bytes.len() <= MAX_REPORT_BYTES, "{label} exceeds 16 KiB");
    Ok(bytes)
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

fn page(
    report: &Report,
    boundaries: Option<&BoundariesReport>,
    attachment: Option<&BoundariesReport>,
) -> String {
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
    html.push_str("</ul></section><section><h2>首片当时未测项</h2><p>以下是首片探针运行时的范围记录；后续证据如已提供，将在下方独立展示。</p><ul>");
    for item in &report.not_tested {
        html.push_str("<li>");
        html.push_str(&escape_html(item));
        html.push_str("</li>");
    }
    html.push_str("</ul></section>");
    if let Some(boundaries) = boundaries {
        html.push_str("<section><h2>P0b 原生语义边界</h2><p>以下是后续独立探针的当前证据。已解决请求的旧响应检查证明结果不会被翻转，并未模拟两个响应同时竞争；故意不读取确认只验证不自动重发，不等同真实网络丢包。</p><p>实际 CLI 版本：<code>");
        html.push_str(&escape_html(&boundaries.cli_version));
        html.push_str("</code>；研究参考源码 commit：<code>");
        html.push_str(&escape_html(&boundaries.source_commit));
        html.push_str("</code>。源码 commit 不证明安装二进制的构建来源。</p><ul>");
        for check in &boundaries.checks {
            let (status, class) = match check.status {
                BoundaryStatus::Passed => ("通过", "pass"),
                BoundaryStatus::Failed => ("失败", "fail"),
                BoundaryStatus::Unsupported => ("不支持", "fail"),
            };
            let label = BOUNDARY_CHECKS
                .iter()
                .find(|(name, _)| *name == check.name)
                .map(|(_, label)| *label)
                .expect("validated check name");
            html.push_str("<li><strong class=\"");
            html.push_str(class);
            html.push_str("\">");
            html.push_str(status);
            html.push_str(" · ");
            html.push_str(label);
            html.push_str("</strong>（<code>");
            html.push_str(&escape_html(&check.name));
            html.push_str("</code>）：");
            html.push_str(&escape_html(&check.detail));
            html.push_str("</li>");
        }
        html.push_str("</ul><h3>P0b 尚未验收</h3><ul>");
        for item in &boundaries.not_tested {
            html.push_str("<li>");
            html.push_str(&escape_html(item));
            html.push_str("</li>");
        }
        html.push_str("</ul><p>草稿检查只覆盖原生未发送文本，未验证 picker 保留。Telegram 生产接入尚未实现；手机跨设备访问尚未验收。</p></section>");
    }
    if let Some(attachment) = attachment {
        html.push_str("<section><h2>P0c 已加载线程附加边界</h2><p>隔离探针复用已有 TUI 对已加载线程的原生事件订阅；独立控制客户端 B 只尝试 turn/start，未调用 thread/resume 或 thread/queue/add。观察消费者断开后 CLI 继续完成任务，但来源证明失效；目标卸载后提交被拒绝，未冷加载线程或留下队列项。</p><p>实际 CLI 版本：<code>");
        html.push_str(&escape_html(&attachment.cli_version));
        html.push_str("</code>；研究参考源码 commit：<code>");
        html.push_str(&escape_html(&attachment.source_commit));
        html.push_str("</code>。源码 commit 不证明安装二进制的构建来源。</p><ul>");
        for check in &attachment.checks {
            let (status, class) = match check.status {
                BoundaryStatus::Passed => ("通过", "pass"),
                BoundaryStatus::Failed => ("失败", "fail"),
                BoundaryStatus::Unsupported => ("不支持", "fail"),
            };
            let label = ATTACHMENT_CHECKS
                .iter()
                .find(|(name, _)| *name == check.name)
                .map(|(_, label)| *label)
                .expect("validated attachment check name");
            html.push_str("<li><strong class=\"");
            html.push_str(class);
            html.push_str("\">");
            html.push_str(status);
            html.push_str(" · ");
            html.push_str(label);
            html.push_str("</strong>（<code>");
            html.push_str(&escape_html(&check.name));
            html.push_str("</code>）：");
            html.push_str(&escape_html(&check.detail));
            html.push_str("</li>");
        }
        html.push_str("</ul><h3>P0c 尚未验收</h3><ul>");
        for item in &attachment.not_tested {
            html.push_str("<li>");
            html.push_str(&escape_html(item));
            html.push_str("</li>");
        }
        html.push_str("</ul><p>生产启动整合和授权仍待验证。本机输入可能混入同一轮，来源门会暂扣这轮外发，不能承诺渠道独占。Telegram 生产接入尚未实现，手机跨设备访问尚未验收。</p></section>");
    }
    html.push_str("</body></html>");
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

async fn index(State(state): State<Arc<PageState>>) -> Response {
    response(
        "text/html; charset=utf-8",
        page(
            &state.first,
            state.boundaries.as_ref(),
            state.attachment.as_ref(),
        ),
    )
}

async fn api_report(State(state): State<Arc<PageState>>) -> Response {
    response(
        "application/json; charset=utf-8",
        serde_json::to_string(&state.first).expect("validated report is serializable"),
    )
}

async fn api_boundaries(State(state): State<Arc<PageState>>) -> Response {
    response(
        "application/json; charset=utf-8",
        serde_json::to_string(
            state
                .boundaries
                .as_ref()
                .expect("route requires boundaries"),
        )
        .expect("validated boundaries report is serializable"),
    )
}

async fn api_attachment(State(state): State<Arc<PageState>>) -> Response {
    response(
        "application/json; charset=utf-8",
        serde_json::to_string(
            state
                .attachment
                .as_ref()
                .expect("route requires attachment"),
        )
        .expect("validated attachment report is serializable"),
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
    let bytes = read_report(&args.report, "report")?;
    let report: Report = serde_json::from_slice(&bytes).context("parse report")?;
    validate(&report)?;
    let boundaries = if let Some(path) = &args.boundaries_report {
        let bytes = read_report(path, "boundaries report")?;
        let report: BoundariesReport =
            serde_json::from_slice(&bytes).context("parse boundaries report")?;
        validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS)?;
        Some(report)
    } else {
        None
    };
    let attachment = if let Some(path) = &args.attachment_report {
        let bytes = read_report(path, "attachment report")?;
        let report: BoundariesReport =
            serde_json::from_slice(&bytes).context("parse attachment report")?;
        validate_semantic(&report, "native-attachment-boundaries", &ATTACHMENT_CHECKS)?;
        Some(report)
    } else {
        None
    };
    let listener = tokio::net::TcpListener::bind((args.listen, args.port))
        .await
        .context("bind acceptance listener")?;
    let address = listener.local_addr()?;
    println!("V3 read-only acceptance: http://{address}/");
    let mut app = Router::new()
        .route("/", get(index))
        .route("/api/report", get(api_report));
    if boundaries.is_some() {
        app = app.route("/api/boundaries", get(api_boundaries));
    }
    if attachment.is_some() {
        app = app.route("/api/attachment", get(api_attachment));
    }
    let app = app.with_state(Arc::new(PageState {
        first: report,
        boundaries,
        attachment,
    }));
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

    fn boundaries() -> BoundariesReport {
        BoundariesReport {
            schema_version: 1,
            kind: "native-semantic-boundaries".into(),
            cli_version: "codex-cli 0.156.1".into(),
            source_commit: "2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4".into(),
            checks: BOUNDARY_CHECKS
                .iter()
                .map(|(name, _)| BoundaryCheck {
                    name: (*name).into(),
                    status: BoundaryStatus::Passed,
                    detail: "synthetic proof".into(),
                })
                .collect(),
            not_tested: vec!["Actual Telegram delivery".into()],
        }
    }

    fn attachment() -> BoundariesReport {
        BoundariesReport {
            schema_version: 1,
            kind: "native-attachment-boundaries".into(),
            cli_version: "codex-cli 0.156.1".into(),
            source_commit: "2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4".into(),
            checks: ATTACHMENT_CHECKS
                .iter()
                .map(|(name, _)| BoundaryCheck {
                    name: (*name).into(),
                    status: BoundaryStatus::Passed,
                    detail: "synthetic proof".into(),
                })
                .collect(),
            not_tested: vec!["Real Telegram delivery".into()],
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
        let html = page(&report, None, None);
        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;) &amp; more&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn boundaries_schema_is_strict_and_spaced_cli_version_is_valid() {
        let mut report = boundaries();
        assert!(validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS).is_ok());
        report.checks.pop();
        assert!(
            validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS).is_err()
        );
        report.checks.push(report.checks[0].clone());
        assert!(
            validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS).is_err()
        );
        report.checks[9].name = "unknown".into();
        assert!(
            validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS).is_err()
        );
        let mut value = serde_json::to_value(boundaries()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BoundariesReport>(value).is_err());
        let mut value = serde_json::to_value(boundaries()).unwrap();
        value["checks"][0]["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BoundariesReport>(value).is_err());
        let mut value = serde_json::to_value(boundaries()).unwrap();
        value["checks"][0]["status"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<BoundariesReport>(value).is_err());
    }

    #[test]
    fn boundaries_escape_text_and_label_prior_untested_scope() {
        let mut report = boundaries();
        report.checks[0].detail = "<script>alert('x') & more</script>".into();
        let html = page(&self::report(), Some(&report), None);
        assert!(html.contains("首片当时未测项"));
        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;) &amp; more&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("codex-cli 0.156.1"));
    }

    #[test]
    fn attachment_schema_rejects_wrong_kind_names_status_and_extra_fields() {
        let report = attachment();
        assert!(
            validate_semantic(&report, "native-attachment-boundaries", &ATTACHMENT_CHECKS).is_ok()
        );
        assert!(
            validate_semantic(&report, "native-semantic-boundaries", &BOUNDARY_CHECKS).is_err()
        );
        let mut missing = attachment();
        missing.checks.pop();
        assert!(
            validate_semantic(&missing, "native-attachment-boundaries", &ATTACHMENT_CHECKS)
                .is_err()
        );
        let mut duplicate = attachment();
        duplicate.checks[4].name = duplicate.checks[0].name.clone();
        assert!(
            validate_semantic(
                &duplicate,
                "native-attachment-boundaries",
                &ATTACHMENT_CHECKS
            )
            .is_err()
        );
        let mut unknown = attachment();
        unknown.checks[4].name = "unexpected".into();
        assert!(
            validate_semantic(&unknown, "native-attachment-boundaries", &ATTACHMENT_CHECKS)
                .is_err()
        );
        let mut value = serde_json::to_value(attachment()).unwrap();
        value["checks"][0]["extra"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BoundariesReport>(value).is_err());
        let mut value = serde_json::to_value(attachment()).unwrap();
        value["checks"][0]["status"] = serde_json::json!("skipped");
        assert!(serde_json::from_value::<BoundariesReport>(value).is_err());
    }

    #[test]
    fn attachment_page_escapes_details_and_old_page_stays_available() {
        let first = report();
        let old = page(&first, None, None);
        assert!(old.contains("首片当时未测项"));
        assert!(!old.contains("P0c 已加载线程附加边界"));
        let mut attachment = attachment();
        attachment.checks[0].detail = "<script>alert('x') & more</script>".into();
        let html = page(&first, Some(&boundaries()), Some(&attachment));
        assert!(html.contains("P0b 原生语义边界"));
        assert!(html.contains("P0c 已加载线程附加边界"));
        assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;) &amp; more&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn oversized_report_is_rejected_without_reading_it_all() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.json");
        std::fs::write(&path, vec![b'x'; MAX_REPORT_BYTES + 10]).unwrap();
        assert!(read_report(&path, "report").is_err());
    }
}
