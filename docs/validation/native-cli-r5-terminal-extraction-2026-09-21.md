# R5 增量：终端工具独立化与调试入口补漏

日期：2026-09-21。分支 `codex/native-cli-live-workbench`，HEAD `9301724` 加当前工作树；未提交、推送或发布。阶段状态仍由[实施计划](../v2-implementation-plan.md)维护，本文不代表 R5 整片通过。

## 1. 实现与边界

新工作台此前通过旧 Session 目录引用通用终端工具，导致删除旧目录前仍有源码依赖。此次将工具独立放置，保留已有实现与测试：

| 内容 | 当前文件 | 验证 |
| --- | --- | --- |
| VT 快照、输出过滤与终端能力应答 | [Rust 终端工具](../../src/terminal/mod.rs) | 除共享配置的引用路径外，与迁移前字节一致；新工作台与旧入口各执行 13 项测试 |
| xterm 主题与配置 | [主题](../../web/src/terminal/terminalTheme.ts)、[配置](../../web/src/terminal/terminalProfile.json) | 实现与测试按原内容移动，无颜色或输入策略变化 |
| 终端写入与背压协调 | [写入协调器](../../web/src/terminal/terminalWriteCoordinator.ts) | 新旧面板引用同一份实现，前端全量测试覆盖 |
| 新旧边界 | [架构测试](../../src/architecture.rs) | 新工作台及共享前端工具不能引用旧 Session/Controller 路径；Rust 共享工具不能依赖控制、存储或工作台上层 |

迁移前新增边界测试出现预期失败：新工作台仍从 `../session/` 包含实现；迁移后通过。全量架构检查另发现旧历史 fixture 的 `Importer` 被当作生产依赖，已用文件级 `#![cfg(test)]` 明确测试边界，检查器只对该编译器约束的文件跳过生产依赖检查，不按文件名放行。

旧 Session/Controller、`/v2` 控制路由和旧启动装配尚未删除；默认 `cargo run` 入口未切换。没有新增兼容空壳、修改旧数据库或删除 audit。

复核[退出清理增量](native-cli-r5-process-cleanup-2026-09-21.md)时，还发现早期 [R1 调试示例](../../examples/r1-debug.rs)未接入 Rust 浏览器所有者。现已改用同一 `ProbeProcess`，并为官方 CLI 显式设置临时 HOME/USERPROFILE。[NativeProbe](../../src/workbench/test_native.rs)也使用调用方的私有目录作为 HOME/USERPROFILE/CODEX_HOME，避免 CLI 支持文件继承真实用户目录。这是测试/示例隔离修复，没有改动产品配置路径。

## 2. 验证结果

```bash
npm test --prefix web
(cd web && npx --no-install tsc --noEmit && npm run build)
cargo test --locked --offline --bin codex-observerd architecture::
cargo test --locked --offline --bin codex-observerd session::terminal::
cargo test --locked --offline --lib terminal_screen::
cargo test --locked --offline --bin codex-observerd http::legacy_history_tests::
cargo test --locked --offline --lib installed_cli_request_metadata_matches_native_user_events_and_separates_title_thread -- --ignored --nocapture
cargo fmt --all -- --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo build --locked --offline --all-targets
git diff --check
```

- 前端 25 文件、171 项测试通过；TypeScript 与 Vite 构建通过。
- 架构 3 项、新旧终端各 13 项、schema 20 只读副本 1 项通过；保留终端安全过滤、Unicode、样式和不完整快照等断言。
- 安装版 CLI 0.155.1 在临时 HOME/CODEX_HOME、临时项目和本机合成模型下完成 2 次原生提交。2 条用户事件、同一线程中不同轮次、1 条独立标题请求及请求元数据关联均通过，之后正常退出。该测试未使用浏览器，也不验证系统中文输入法。
- fmt、Clippy 全 targets、全 targets 构建及 diff 空白检查通过，正式 `target/debug/codex-view` 已包含更新后的前端资源。首次 Rust 检查被过期前端资源门槛阻止；先完成前端构建后重跑通过，没有绕过构建检查。
- 迁移文件与迁移前记录的 SHA-256 核对一致，Rust 文件仅将配置引用路径还原后比较。未覆盖既有终端修复。

此次没有重跑浏览器验收；R1 示例此次完成编译及所有者调用复核，所有者的正常/超时回收行为沿用前一增量的独立 Chrome 回归证据，不声称 R1 示例本轮端到端通过。

## 3. 真实输入法验收尝试与剩余条件

在临时用户目录启动了正式工作台与官方 CLI，使用本机合成模型准备系统输入法验收。浏览器工具拒绝本地 `file://` 配对入口；随后直接访问工作台 HTTP 根页面也返回 `net::ERR_BLOCKED_BY_CLIENT`，未进入终端，未通过其他自动化方式绕过限制。

本次模型主请求数为 0。已关闭创建的错误页、停止隔离监督进程并回收 Launcher；随后进程盘点确认所属 CLI 同样退出、临时监听关闭。临时目录由监督进程清理，未留下常驻调试服务。

因此，拼音候选、组合过程中 Enter 仅确认文字、Shift+Enter 多行及刷新后原生草稿仍待实际输入法操作确认。源码直接使用 xterm 或自动化粘贴成功，均不能替代这项证据。已向用户请求手工结果；手机软键盘及接入范围也仍待用户决定，未默认从 R5 删除。

按用户授权清理的两组旧 App Server、原型服务、测试 API、两组测试 Chrome 及已记录的辅助进程均已退出；复查原型端口 52610、测试 API 端口 3100 和本次输入法验收端口均关闭。正常应用进程及另一个工作目录中的 Python 服务保留，不根据进程名称或 PPID=1 批量关闭。

本次没有数据库 migration、用户配置或公开 API 变化，没有切换用户服务。R5 仍缺真实输入法结果、手机范围/设备验收及旧控制完整退役。

2026-09-21 后续用户反馈：电脑端右侧命令行中文输入正常。已在桌面支持矩阵中记为人工试用结果；此反馈未逐项列举组合 Enter/Shift+Enter/刷新，不虚构逐项操作记录。用户进一步询问局域网手机访问，实施计划已修正为先补接入能力，再提供手机验收入口，不继续把缺少手机自动化工具当作产品接入的替代问题。
