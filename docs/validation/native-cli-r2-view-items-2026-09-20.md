# R2 增量验证：统一 ViewItem 与快照接续

日期：2026-09-20。结论：用户、模型、工具和 notice 的快照/实时契约已统一；丢失增量后重新快照、稳定节点/焦点、未知类型和安装版 CLI 工具回归通过。**仅本增量通过，R2 整片仍为 In progress**；唯一进度见[实施计划](../v2-implementation-plan.md)。

## 1. 环境与边界

- 分支 `codex/native-cli-live-workbench`，HEAD `930172493c163a903621ca14c540d1ca10dd4f3e` 加当前未提交工作树；没有 commit、push、Issue/PR 或发布操作。
- 安装版官方 CLI `0.154.0`；系统 Chrome `153.0.8010.50`，headless，不下载浏览器。官方只读参考仓库本次核对仍为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`；本增量是项目契约变更，没有新的官方协议结论。
- 当前 `target/debug/codex-view` SHA256：`357c5e41acd7eec32e96bed47e609683922e42490dc8a4ed0bd199b95fd1898b`。
- 使用合成模型服务、临时项目及临时 CODEX_HOME；CLI 真实执行读写与命令，模型返回值由 fixture 控制。无真实模型请求、用户输入重放或真实用户配置写入。
- 原用户调试进程未重启，旧 `/v1`、数据库、原版 CLI 和全局配置未变；没有 migration。新内存阅读草案同步升级为 schemaVersion=2，旧页面需刷新加载对应脚本。

## 2. 交付行为

[ADR 0043](../decisions/0043-unified-reading-items.md)与[详细设计 §4.3](../codex-native-cli-workbench-detailed-design.md#chat-item-contract)记录实际契约：

- snapshot 只有 `items: ViewItem[]`，不再输出 tools/userMessages。消息 author 明确为 user/assistant；工具与提示有独立 kind，itemKey、revision、orderIndex、来源与不完整性随项目保留。
- 所有项目首条为 item.replace。item.patch 只允许 text/contentKey 或 arguments；无项目、版本缺口、字段或类型冲突要求快照，不根据增量内容创建猜测的角色。
- 权威全文、模型名/正文终态、工具结果和来源补齐都替换同一项目。响应结束不结束工具或整个任务，原生结果仍按原规则匹配，缺失事实不补成功。
- 后端保留独立有界来源聚合，输出和前端状态只有同一套项目；模型/用户/工具选择器即时派生，不新建长期内容副本。
- 未知合法 envelope 显示固定 notice，未知 payload 丢弃；已知内容沿用 Markdown 清理与文本转义。非法 envelope 或无法应用的 patch 不推进已应用 cursor。
- 右侧单页直接可输入，输出始终可读；仅另一页面正在输入时显示“在此输入”。保留原生 truecolor 和关闭装饰动画的设置，无新增锁定/释放按钮。

## 3. 自动化与浏览器结果

| 验证 | 断言与实际结果 |
| --- | --- |
| 混合 wire 契约 | 真实 LiveHub 同时发布用户、模型、工具、notice，独立测试消费者重放所有 replace/patch，与最终快照逐项目完全相等；无旧 tool/user 事件，参数与正文补丁分别有 field |
| 消息状态 | 请求/响应模型来源变化增加原项目 revision；失败响应使未完成正文为 incomplete，已有权威全文仍 ended；工具仍 unobserved |
| 前端 reducer | 首个 patch 拒绝；重复 seq 不重复；旧 replace 不回退；contentKey、role、field、revision、sequence 冲突拒绝；两个 content index 不串接 |
| 快照与来源 | 从 typed snapshot 接续与不中断的结果一致；旧 schema、epoch 和重复 itemKey 拒绝；迟到原生提交按明确 turn 归组，相同新提交保持两条，WS create 不按次序归属 |
| Chrome 丢事件 | 浏览器故意跳过一条真实 item.patch，下一事件检测 sequence gap；快照 GET 次数增加，恢复漏失正文与新模型来源。原模型/工具 DOM 引用相同，参数展开与 xterm 焦点保留 |
| Chrome 未知类型 | 合成快照附加未来未知类型及 HTML payload，只呈现固定 notice；未知 payload 不进入正文、脚本未执行。刷新后模型/工具数量不变，同一 PID/epoch |
| 安装版 CLI 工具 | Code Mode 与直接命令分别执行读文件、写临时文件和退出 7。每种模式 1 用户、4 模型、3 工具卡，无重复；真彩色、静态欢迎界面、同进程/刷新/窄屏和定义读取回归通过 |
| 安装版 CLI 文件修改 | 新增/修改/移动/删除及拟议 Diff 正常；原生完成/执行失败原位补齐；前置校验失败保持 result_observed。4 个主请求、1 用户、4 模型、3 工具卡，stdout/stderr 分区不变 |
| 迟到命令结果 | 无下一请求的迟到退出、真实 write_stdin 轮询与长输出三种场景均更新原卡，保留同一 CLI，不重发输入 |
| R0 诊断页 | 升级为统一消息后仍安全显示中间正文/最终替换并刷新；2 次快照/流连接，pageErrors=0 |

所有上述 Chrome 场景 `pageErrors=0`。界面探针仍检查 1024/736/320px 宽度、键盘展开与容器溢出；这不代表真实手机/IME 验收。图片为合成测试证据，不含真实会话。

## 4. 检查与复现

- `cargo test --lib`：**158 passed / 19 ignored**，7.09s。ignored 的安装版 CLI/Chrome 测试另按下列命令显式执行，不把 ignored 计为通过。
- 前端完整 Vitest：**133 passed / 19 files**，1.48s；其中工作台/Markdown/终端写入相关 60 项也单独通过。
- TypeScript、Vite 构建、`cargo fmt --all -- --check`、`cargo clippy --lib --bin codex-view --examples -- -D warnings`、正式 binary 构建通过。
- 显式 Chrome identity/gap/unknown 测试 2.81s；CLI Code Mode/command 10.28s，patch 4.51s，迟到/轮询/长输出 12.66s；R0 诊断 Chrome 0.91s。
- 本次没有做性能分位数重测，不沿用旧性能值宣称新契约开销。7 份文档的本地链接/锚点、代码块与 git diff whitespace 检查通过。

```sh
npm run build --prefix web
(cd web && ./node_modules/.bin/tsc --noEmit && npm test)
cargo test --locked --offline --lib
cargo clippy --locked --offline --lib --bin codex-view --examples -- -D warnings
cargo build --locked --offline --bin codex-view
cargo test --locked --offline --lib workbench_keeps_dom_keys_when_final_items_acquire_wire_ids -- --ignored --nocapture
cargo test --locked --offline --lib native_code_and_command_cards_match_actual_read_edit_and_failed_results -- --ignored --nocapture
cargo test --locked --offline --lib native_patch_diff_matches_actual_add_update_move_delete_and_failed_result -- --ignored --nocapture
cargo test --locked --offline --lib browser_keeps_the_original_command_card_through_late_exit_and_native_polling -- --ignored --nocapture
cargo test --locked --offline --lib system_chrome_observes_safe_midstream_content_replacement_and_refresh -- --ignored --nocapture
```

loopback/Chrome 测试需要允许本地监听；临时 fixture 自行清理。可设置 WORKBENCH_TEST_SCREENSHOT 为自己的本地前缀保存截图，命令不输出私有配对入口。

## 5. 截图与后续

[未知类型安全提示与稳定卡片](native-cli-r2-view-items-2026-09-20.identity.png)、[Code Mode](native-cli-r2-view-items-2026-09-20.code.complete.png)、[直接命令与失败结果](native-cli-r2-view-items-2026-09-20.command.complete.png)、[文件结果](native-cli-r2-view-items-patch-2026-09-20.complete.png)、[拟议 Diff](native-cli-r2-view-items-patch-2026-09-20.diff.png)、[320px 窄屏](native-cli-r2-view-items-patch-2026-09-20.narrow.png)。

核心变更位于 [后端统一序列化](../../src/workbench/live/view.rs)、[LiveHub](../../src/workbench/live.rs)、[前端 typed 项目](../../web/src/workbench/viewItems.ts)、[事件 reducer](../../web/src/workbench/reading.ts)和 [Chrome 故障探针](../../web/e2e/r2-identity-probe.cjs)。既有工具匹配、读取预算与 PTY 生命周期保持原逻辑，仅更新消费统一契约的位置。

下一项是 R2 的 compact/fork/steering/续传与并发、WS 请求关系，以及其他工具/取消事实的剩余 C03–C10 验收。尚未完成的关联不按时间、文本或 ID 前缀补齐。R2 整片通过后暂停供用户实际试用，再决定是否进入 R3。
