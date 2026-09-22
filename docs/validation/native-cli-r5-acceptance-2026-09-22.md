# R5 收尾验收与交付依据（2026-09-22）

用户认可连续文件阅读后要求继续下一部分，本次执行最终集成验收与交付收尾。阶段状态只在[实施计划](../v2-implementation-plan.md)维护；当前可试用能力与限制集中在[支持范围](../codex-native-cli-workbench-support.md)。本报告串联已执行的证据，未验证平台/provider 不扩大为已支持。

## 1. 当前基线

- 分支 `codex/native-cli-live-workbench`，HEAD `930172493c163a903621ca14c540d1ca10dd4f3e` 加当前 R0–R5 未提交工作树；未改写用户提交。
- macOS 26.5.2 / arm64，官方 CLI `0.155.1`，系统 Chrome `153.0.8010.53`，Rust `1.95.0`。
- 官方源码参考仍为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`，只读检查 commit；本轮不根据其他未验证源码推导兼容性。
- 自动化使用临时 HOME/USERPROFILE/CODEX_HOME、合成 Git 项目、合成模型上游与独立浏览器。未启动用户常驻调试实例、未撤销真实手机授权、未修改全局配置或原生历史。
- 本轮没有再次发送真实模型任务；实际 provider、图片及原生审批/追问联合行为沿用 [2026-09-21 正式产物证据](native-cli-r5-desktop-2026-09-21.md#2-正式产物与真实模型)。其模型/provider、CLI 与实验限制不改写为本轮重新测得。

## 2. 验收中发现并修复的问题

**调用记录打开后立即 Esc 无效。** 浏览器已显示新面板，但被动 effect 尚未把焦点从旧按钮移动到面板，快速按 Esc 落在面板外。Chrome 原生交互验收稳定停在 `keyboard-navigation-close-calls`；新增同步挂载测试得到“旧按钮不等于关闭按钮”的失败。手机接入展开面板具有相同的被动焦点设置，针对性测试也复现。

修复仅将两处打开焦点设置改为 layout effect，并使用 `preventScroll`；数据读取/二维码/轮询继续在原有异步 effect 中。焦点仅在打开时进入面板，后续数据/父组件更新不重新抢焦点。`CallInspector.test.tsx` 与 `AccessPanel.test.tsx` 验证即时 Escape 和后续焦点保留；真实 Chrome 验证调用面板 Esc 返回用量入口、接入面板 Esc 返回手机接入按钮，终端不接收页面导航按键。

**旧键盘脚本未随导航入口扩展。** 原 R1 脚本假定首个 Tab 就到“终端”，并跳过后来加入的文件/搜索/Git。更新为当前头部与导航的实际顺序，仍逐个验证真实 Tab 焦点与 Space/Enter 行为，没有用直接 `.focus()` 代替可达性断言。各阶段名称细化，后续失败可以区分入口顺序、面板打开/关闭与原生输入。

未放宽产品权限、测试超时、断言或性能阈值。新增用例先复现再修复；其余已通过功能只做必要回归。

## 3. 本轮自动化结果

| 范围 | 结果 |
| --- | --- |
| Rust 全量 all-targets，4 个测试线程 | 341 项通过；37 项默认忽略包含显式 CLI/Chrome 环境测试、子进程 helper 与历史容量实验，不计作通过；下列 10 项环境测试单独执行 |
| 正式 Launcher（3 项） | SIGINT/SIGTERM/SIGHUP、配置/profile/目录覆盖、真实用户/模型消息、连续文件阅读、刷新/接管、原生退出、网页停止与重复停止、顺序回到普通 CLI 均通过；旁侧无关进程保留 |
| 原生 picker/审批/追问/断线（1 项） | 真实安装版 CLI，4 个合成模型请求；丢失 ACK、刷新不重发，审批不自动代答，追问选项精确返回；同一 CLI、0 页面错误/终端 fault；含即时 Esc 回归 |
| 历史跨启动（1 项） | 两次独立 Run、3 个合成请求、命令仅执行一次；第二次恢复已保存工具结果，不自动 resume 或重放输入 |
| 原生拒绝/取消（1 项） | 参数错误、权限拒绝、未知工具、部分参数与正文取消继续区分；同一 CLI 后续输入可用，未把取消参数伪装为实际工具执行/取消 |
| 请求上下文（1 项） | 按需读取、系统/developer/工具信息、脱敏与字面安全、逐响应用量、过期游标保护；0 页面错误，同一 PTY |
| 设备接入（1 项） | QR 解码、重复扫码复用 Cookie、刷新/重开同码、多地址同 Run、普通 HTTP、中文输入、断线/刷新/撤销、本机继续使用与即时 Esc 均通过；0 页面错误。启用到 QR 581ms、重新开启 939ms，仅为单次合成样本 |
| 设置/历史清理（1 项） | 同页展开/草稿/外部冲突、单条/批量删除、关闭默认值与设置返回、保留期限、其他页面失效、1024/736/320、同一终端均通过；0 页面错误 |
| 保存故障（1 项） | 保存水位冻结，网页与终端继续接收；恢复后明确缺口，0 页面错误 |
| V1 历史 Chrome | 10 项通过：390/820/1280/1440、搜索/定位/raw、配对、迟到响应、旧控制能力字段不恢复控制入口，零 `/v2` 请求 |

前端最终 24 个测试文件 / 156 项通过，TypeScript、Vite build、fmt、Clippy all-targets `-D warnings` 及 debug/release 双入口构建通过。另将 `WORKBENCH_TEST_LAUNCHER` 指向 release 产物，重新执行 3 项正式 Launcher 测试通过（同样 0 页面错误/终端 fault、配置保留与退出清理成立）。所有集成回归串行执行，避免测试自身抢占资源影响断线/退出的时间断言；V1 Playwright 使用其现有独立浏览器测试并发配置。

## 4. 需求与验收证据闭环

| 验收组 | 需求 | 证据与范围 |
| --- | --- | --- |
| P01 接入 | RQ01 | [R0](native-cli-r0-progress-2026-09-18.md)、本轮正式 Launcher；当前目录普通 CLI，无本项目外部 App Server |
| P02 转发 | RQ02、RQ05 | R0、全量 HTTP/SSE/WS/取消/压缩/未知字节测试；真实接入只覆盖支持页所列 provider |
| P03 终端 | RQ04、RQ06 | 本轮安装版 CLI 原生交互、刷新/单写/接管/退出；用户已确认桌面中文输入，未逐项反馈的输入法组合不伪造结果 |
| P04 语义 | RQ02、RQ03、RQ05 | [R2](native-cli-r2-acceptance-2026-09-20.md)、本轮拒绝/取消/上下文；角色、参数与执行、响应与任务分别表达 |
| P05 阅读 | RQ02、RQ09 | R1/R2、全量阅读锚点/身份/最终替换及本轮原生双消息验证 |
| P06 历史 | RQ06 | [R3](native-cli-r3-acceptance-2026-09-20.md)、本轮跨启动/保存故障；只恢复可信前缀，缺口和尾部明确 |
| P07 隔离 | RQ02、RQ06 | [R5 性能矩阵](native-cli-r5-desktop-2026-09-21.md#4-正式页面性能与故障)、本轮慢 recorder/管理/页面与满队列单测；不把观察暂停当作 Recorder 暂停 |
| P08 安全 | 跨需求 | 本轮全量 Host/Origin/CSRF/配对/固定 upstream/脱敏/路径/旧接口负向测试；V1 保持只读 |
| P09 工作区 | RQ07 | [R4](native-cli-r4-acceptance-2026-09-20.md)、[连续文件验收](native-cli-continuous-files-2026-09-22.md)及本轮正式 Launcher 文件阅读；固定根与资源边界不变 |
| P10 实机与退役 | RQ08 | [真实模型/图片](native-cli-r5-desktop-2026-09-21.md#2-正式产物与真实模型)、用户手机接入/输入确认、[旧内核退役](native-cli-r5-kernel-retirement-2026-09-21.md)及本轮旧 schema/audit/进程回归；图片路径以外的方式不扩为支持 |
| P11 交互 | RQ09 | 角色/类型、Git 与对话同屏、只读文件、历史/设置保持终端；本轮键盘导航和即时 Esc，先前窄屏截图继续有效 |
| P12 历史管理 | RQ10 | [R3.1](native-cli-history-settings-2026-09-20.md)、全量 8 个崩溃点/锁/路径/跨项目保护及本轮 Chrome 单条/批量/保留期限 |
| P13 统一配置 | RQ11 | R3.1、[独立用户目录](native-cli-user-directory-2026-09-20.md)、本轮 JSON/草稿/冲突/默认关闭清理与 CLI 覆盖验证 |
| P14 设备 | RQ12 | [接入](native-cli-device-access-2026-09-21.md)、[固定码](native-cli-fixed-pairing-2026-09-21.md)、用户真机输入确认及本轮 Chrome；真机锁屏/双网络等后续优化按用户要求暂缓 |

上述需求均有对应实现和验证/明确边界。未验证 provider、其他桌面平台、手机锁屏/双网络及图片剪贴板/相册仍列为限制，不生成新的本轮实现清单，也不宣称跨平台/全认证兼容。

性能使用原始证据的日期与测点：2026-09-21 正常三例接收→Paint p95 ≤96.157ms；长文本 p99 为 124.337ms，故障展示不满足正常目标但网络样本送达。2026-09-22 连续文件测试 50 万行末尾正文 DOM 39 行、GC 后页面堆约 11.51MiB，关闭后约 7.25MiB。两种实验不同，不能混成当前全负载性能承诺；本轮没有重跑其所有场景或得出新的性能分位数。

## 5. 交付、修改与复现

本轮产品源代码修改限定于 `CallInspector.tsx` 与 `AccessPanel.tsx` 的打开焦点时序；对应新增/补充 `CallInspector.test.tsx`、`AccessPanel.test.tsx`、原生导航与设备浏览器脚本。README、支持范围页、实施计划和本报告同步，旧桌面报告增加后续入口而保留当时事实。没有迁移、配置 schema、API、代理、原生权限或旧数据格式变化。

```sh
npm test --prefix web
./web/node_modules/.bin/tsc --noEmit -p web/tsconfig.json
npm run build --prefix web
cargo test --locked --offline --all-targets -- --test-threads=4
cargo test --locked --offline --lib workbench::proxy::tests::native_cli::launcher:: -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib product_browser_native_picker_approval_question_and_short_disconnect -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib product_history_survives_restart_without_resuming_or_replaying_native_input -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib native_tool_refusals_and_cancelled_parameters_remain_distinct_from_execution_facts -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib browser_reads_context_on_demand_without_losing_terminal_or_reading_state -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib browser_device_access_qr_http_stream_input_and_revocation -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib browser_settings_expand_save_conflict_and_collapse_keep_reading_and_terminal -- --ignored --test-threads=1 --nocapture
cargo test --locked --offline --lib browser_reports_save_failure_and_recovery_while_terminal_and_reading_continue -- --ignored --test-threads=1 --nocapture
PLAYWRIGHT_USE_SYSTEM_CHROME=1 npm run test:e2e --prefix web
cargo fmt --all --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo build --locked --offline --release --bins
```

GitHub Issue/PR、提交、推送、tag 与发布不在本次授权中，均未执行；工作树保留当前分支及此前改动。当前验收范围之外的能力须在未来用户选择后另行设计，不恢复旧内核或直接提前实现 V3。


## 6. 本地交付产物

release 构建并经过正式入口复验；不是已发布的 tag/release，也没有替换用户正在运行的实例。当前两份产物的 SHA-256：

| 产物 | 字节数 | SHA-256 |
| --- | --- | --- |
| `target/release/codex-view` | 14,179,536 | `e4602c0589ea8b96eb3b27d163b342e3e96bc0218d81ed00145967c2a45a3e7c` |
| `target/release/codex-observerd` | 9,982,272 | `80765f4299db87ce9143f30763ebaf22f0ed0bf0561ddc302126b929f1cf57dc` |

```sh
WORKBENCH_TEST_LAUNCHER="$PWD/target/release/codex-view" cargo test --locked --offline --lib workbench::proxy::tests::native_cli::launcher:: -- --ignored --test-threads=1 --nocapture
```

最终只读进程盘点：没有本项目浏览器探针 Node 或临时 Playwright Chrome 遗留；未向其他用户进程发送信号。6 份交付文档的 237 个本地链接/锚点及代码块检查通过，Git diff 检查通过。

按用户“每阶段完成后暂停试用”的约定，本轮到此交付。最小人工试用为：从项目目录启动新 release，进入原生对话，展开调用记录/手机接入后立即 Esc，再确认文件连续阅读、历史及正常退出。下一步不自动开启新功能片；收到用户后续要求再处理反馈或本地提交/PR准备。
