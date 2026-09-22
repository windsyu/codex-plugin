# CLI 升级兼容验证：取消精确版本锁定

日期：2026-09-20。对应 [ADR 0045](../decisions/0045-cli-version-compatibility.md)、[详细设计 §2](../codex-native-cli-workbench-detailed-design.md#2-启动与-provider-配置)与[实施计划](../v2-implementation-plan.md)。本次是 R3 试用修复，不开始 R4。

## 1. 问题与结论

用户升级官方 CLI 后，`codex-view` 报 `installed CLI version is not validated; expected 0.154.0`。原检查只接受 `codex-cli 0.154.0`，没有实际发现新版本的接入故障。先新增新版回归并确认旧实现对 `0.155.1` 失败，再移除精确相等条件。

修正后，受测版本只是诊断证据；其他版本提示后继续启动，不要求降级、修改配置或传 bypass 参数。保留版本探测的成功退出、输出格式、大小和超时检查，实际配置/路由错误另行报错。启动 ready JSON 新增安全的 `cliVersion` 字段。

## 2. 环境与范围

- 项目基线 `930172493c163a903621ca14c540d1ca10dd4f3e`，分支 `codex/native-cli-live-workbench`，当前本地工作树；未 commit/push，既有修改保留。
- macOS，本机 `/opt/homebrew/bin/codex` 报 `codex-cli 0.155.1`；Chrome `153.0.8010.50`。未安装/升级/降级 CLI 或下载浏览器。
- 正式 `target/debug/codex-view`、临时项目/`CODEX_HOME`、合成 HTTP/SSE 模型服务；实际运行安装版原生 CLI 和本机 Chrome。未提交真实模型任务，未写真实用户 home。
- `0.154.0` 保留为旧验收基线；本报告的新增证据来自 `0.155.1`。旧日期报告的版本和测试结果不改写。

## 3. 回归结果

| 检查 | 断言与结果 |
| --- | --- |
| 升级与版本形态 | `0.155.1`、`0.156.0`、`1.0.0`、`0.155.0-alpha.1`、`0.155.1+build.42`、`0.153.0` 均被识别；没有最低版本或 major 准入 |
| 未来版本正式装配 | 合成可执行文件报告 `9.0.0`，Launcher 正常启动、不改原配置，drop 后回收自有子进程；无需版本覆盖参数 |
| 负向探测 | 非法输出、非零退出、超过 4096 字节与挂起分别失败；任意输出正文不进入错误，挂起在 5 秒预算内超时 |
| 正式 Launcher | 命名 profile、原生模型/认证路由保持；两轮相同中文输入分别成消息，流式中间态、刷新、双页接管与退出清理通过 |
| 原生交互 | picker 取消、审批、Plan 追问、短断线、丢 ACK 和刷新通过；重连不自动发送输入、批准命令或提交答案 |
| 显式 resume | 新 runEpoch、同一原生 Thread，历史进入下一请求；新输入前不请求模型、不重放旧输入 |
| 保存历史 | 两次独立启动、3 次模型请求、1 次原生命令；历史用户/模型/工具/上下文恢复，阅读历史不 resume 或重发输入 |
| 生命周期 | stop、SIGINT、SIGTERM 只结束自有原生进程；重复 stop、终端停止后阅读、无关进程保留通过 |

首轮 5 项安装版产品测试有 4 项通过，1 项停在页面键盘导航：R3 新增了“历史记录”按钮，旧 R1 探针仍预期从“模型请求”直接 Tab 到“运行状态”。已补充历史按钮焦点断言，保留导航不发送原生输入的检查；单独重跑该场景通过。它不涉及产品页面修改，也不是 CLI 协议不兼容。最终 5 项产品场景全部有通过证据，Chrome 页面错误与终端 fault 均为 0。

## 4. 可复现命令与质量检查

沿用已构建的当前前端资源，本次只修改 Rust 与浏览器探针；最终 Rust 二进制已重建。

```bash
cargo test --locked --offline --lib version_probe -- --nocapture
cargo test --locked --offline --lib workbench::launch::tests -- --nocapture
cargo build --locked --offline --bin codex-view
cargo test --locked --offline --lib product_ -- --ignored --nocapture
WORKBENCH_TEST_SCREENSHOT=/private/tmp/codex-cli-01551-native \
  cargo test --locked --offline --lib \
  product_browser_native_picker_approval_question_and_short_disconnect \
  -- --ignored --nocapture
cargo test --locked --offline --lib
cargo fmt --check
cargo clippy --locked --offline --all-targets -- -D warnings
node --check web/e2e/r1-native-flow-probe.cjs
git diff --check
```

- Launcher 测试 9 项通过；最终库测试 177 通过、25 显式忽略。需安装版/Chrome 的 5 项产品场景另行显式执行；其他 ignored 场景本次未重跑，不算新增验证。
- Rust binary build、fmt、Clippy 全 targets `-D warnings`、探针语法与 diff 空白检查通过。前端产品代码未变，本次不重复前端单测或构建。
- 8 份相关文档的 208 个本地链接/锚点、标题层级与代码块检查通过。
- loopback 场景使用本机执行权限，进程与临时服务由测试清理；最终进程检查未发现遗留工作台或本项目测试进程，没有为本次修复新开持续调试服务。

## 5. 修改、限制与交付

实现位于 [Launcher](../../src/workbench/launch.rs)、[版本与生命周期回归](../../src/workbench/launch/tests.rs)、[ready 输出](../../src/bin/codex-view.rs)和[原生交互探针](../../web/e2e/r1-native-flow-probe.cjs)。README、执行指南、核心约束、方案、详细设计、ADR 与实施计划同步版本策略。

本次不涉及存储 migration 或用户配置变更，旧 `/v1`、provider/认证/真实 WS 支持边界不变。`0.155.1` 的证据使用合成模型，不宣称重新跑过真实上游性能矩阵；未来版本仍可能有具体参数或协议差异，但不再仅凭版本号拒绝启动。

用户可直接重跑原命令 `/Users/windsyu/magicproject/codex-plugin/target/debug/codex-view` 继续 R3 试用。没有创建/修改远程 Issue 或 PR，R4/R5 保持 Pending。
