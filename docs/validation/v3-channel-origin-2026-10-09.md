# V3 首片：来源隔离与原生完成探针

> 日期：2026-10-09。工作分支：`codex/v3-channel-origin-spike`，基线 `17187ec`。
> 范围：原版 CLI、合成模型、合成渠道的隔离协议验证和纯校验。生产 Telegram、绑定界面、远程审批与恢复尚未实现；不改变 V2 验收。

[文档总索引](../README.md) · [V3 实施计划](../v3-implementation-plan.md) · [候选 ADR 0055](../decisions/0055-v3-channel-native-protocol.md)

## 1. 环境与隔离

实际安装版 CLI 为 `0.156.1`，Rust `1.95.0`，Node `22.23.2`。只读研究 checkout 的 commit 为 `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`；没有证据证明它对应安装版二进制的构建来源。未升级或修改官方 CLI。

[探针](../../examples/v3-native-channel-probe.rs)为每次运行创建独立临时 HOME、USERPROFILE、CODEX_HOME 和项目，使用合成 provider 与合成 bearer；上游只监听 loopback，候选原生协议只监听临时 Unix socket。退出后清理本次专属进程与目录。不读取真实用户配置、会话或登录凭证，不调用真实模型、Telegram 或第三方消息服务。

macOS `/tmp` 的符号链接会影响原生 trust key；探针将项目路径 canonicalize 后再登记 trust，解决首次探针未发模型请求的问题。修正后重新实际运行成功，不把最初失败当作通过。

## 2. 实际通过的协议检查

| 检查 | 实际证据及边界 |
| --- | --- |
| 当前普通启动路径 | `-c` 覆盖下，CLI 经生产 `ProxyServer` 到达合成模型；临时 HOME 内没有 socket。这是当前无法直接附加的证据，不是渠道可用声明 |
| 同一个已有会话 | 候选私有 App Server 与原版 remote TUI 先创建本机上下文，第二 RPC 客户端附加同一个已加载 Thread；没有另开 `exec resume` 或另一会话 |
| 本机输入隔离 | 本机 UserMessage 有结构化身份证据，不纳入渠道 client ID 集合；不依据正文、ANSI 或时间归属 |
| 相同正文 | 两次相同合成文本使用不同 client ID，分别收到准确原生 UserMessage/Turn 回显，最终完成两个不同 Turn |
| 原生来源与完成门 | RPC 前创建 `OriginProof`，按有序 `turn/started`、UserMessage started/completed、client ID、明确 final phase 和原生终态处理；非终态不释放回复，两次终态均匹配合成 final |
| 多次模型响应 | 每条渠道 Turn 先生成合成 `update_plan` 调用，再进行第二次模型响应；两次 `response.completed` 不替代原生 `turn/completed` |

探针中的 ANSI 仅用于合成 TUI 首次就绪和输入一条本机消息，不能证明提交、来源、审批或完成。来源证据来自原生协议与结构化记录。

`turn/completed.items` 是摘要，不能用它证明 Turn 没有本机输入；`clientUserMessageId` 是来源关联，不是原生幂等键。源码研究发现 `turn/start` 可进入已有 Turn，因此不能用忙闲检查后发送来保证渠道独占；同 Turn 混入任何其他 user item 都关闭自动外发。

## 3. 新增纯边界与检查

[来源门](../../src/workbench/channel/origin.rs)每次只处理一个实际提交尝试，内存有界；只允许明确 final 文本，在成功原生整轮结束后释放一次。没有完整用户生命周期、来源混入、ID 冲突、未知终态、捕获缺口、异常顺序或回复超限时不释放。完成身份保留有界 tombstone，满时失效而不淘汰；重复 final item 不覆盖更新结果，同 item 正文变化失效。

[Telegram 校验](../../src/workbench/channel/telegram.rs)只接受准确绑定的 user ID、private chat ID 和非 bot 发送者；按 i64 精确解析身份，转发来源不授予权限。文本及 entity 使用 UTF-16 边界，控制字符、未知内容变体、媒体和编辑更新不能变为任务。callback 只接受本 bot 的可访问消息及有界 opaque reference；task、generation、过期和一次性消费仍属于未实现的任务 actor，不能只凭此校验执行审批。

纯边界测试覆盖同 Turn 本机 steer（包括 final 后但 Turn 完成前）、本机抢先、复用 client ID、缺少 started、回显不一致、重复终态、缺口、失败/中断/未知状态、先有未归属回复、乱序本机输入、已完成 Turn 重开、tombstone 溢出、final 重放，以及 Telegram 身份、Unicode、callback 负例。独立只读复核发现三条来源漏洞，已增加对应回归并修正；复核意见本身不算测试通过。

| 本次检查 | 结果 |
| --- | --- |
| `node scripts/dev.mjs check --ci` | 成功；维护工具 80、前端 234、Rust 462 通过，0 失败；Rust 56 ignored，未计作通过 |
| TypeScript、Web 构建、Rust fmt/Clippy | 通过；固定工具链和 `--locked` |
| 两个 binary 的 debug/release 构建 | 通过，Web 先构建 |
| 新增渠道纯边界 / 验收页专项 | 15 / 2 项通过，计入上述 Rust 总数；不是额外叠加计数 |
| 安装版 CLI 协议探针 | 最终源码实际复跑退出 0，七项检查通过；合成模型，不是 Telegram 实测 |
| 文档 / 工作差异内容检查 | 110 份 Markdown 通过；14 个修改或新增文件未发现禁止二进制；差异空白检查通过 |
| 只读验收 HTTP | 本机通过 Tailnet 地址读取页面与七项报告为 200，`no-store`；POST 为 405 |
| Chrome 页面检查 | 独立浏览器上下文、390×844 手机模拟视口；七项通过可见，宽度无水平溢出。未把模拟视口当作真实手机/Tailnet 验收 |

完整检查的受管产物为 `target/artifacts/89cec747-5367-4b1d-afe1-50f5877e5e63`；最终协议报告为 `target/artifacts/54629e2f-dc6d-4b1a-8c4f-59f4391b0a73`。先前 Clippy 布尔表达式检查失败的任务保留为失败诊断，修正后完整检查与协议复跑成功。产物和原始日志均不入 Git。

这些模块当前由隔离 example 使用，生产 launcher 没有启用渠道入口。未新增依赖、配置、schema 或 migration；旧 `/v1` 不变。

## 4. 复现与只读远程验收

使用开发指南的固定工具链和锁定依赖。专项命令为：

```sh
cargo test --locked --lib workbench::channel:: -- --test-threads=4
cargo test --locked --example v3-channel-acceptance
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked --example v3-native-channel-probe -- --report <登记产物目录>/report.json
```

可用 `WORKBENCH_TEST_CODEX` 显式选择自行安装的官方 CLI；不把维护者机器路径作为贡献者前提。专项不会升级 CLI。报告使用 `0600`、禁止覆盖，只写版本、研究 commit、七项布尔检查和未验范围，不写正文、会话 UUID、路径或凭证。

[验收页](../../examples/v3-channel-acceptance.rs)严格读取该安全汇总，仅提供 `/` 和 `/api/report`，无输入或控制接口；正文 HTML 转义、禁止脚本、响应不缓存。默认监听 loopback，只有明确传入本机 Tailscale 地址才远程可达，不监听 `0.0.0.0` 或修改 Tailscale 设置。

```sh
cargo run --locked --example v3-channel-acceptance -- \
  --report <登记产物目录>/report.json \
  --listen <本机 Tailscale IPv4> --port 0
```

这份汇总不含私人资料，Tailnet 中允许连接该机器的设备可以阅读；不是带会话控制权的设备配对页面。用户手机的 Tailnet ACL、网络和实际可达性须单列验收，不能用本机 HTTP 成功代替。

## 5. 未验与后续

- 原生审批、追问和多客户端响应竞速。
- 实际原生本机/渠道交错 steer、草稿、picker；当前负向来源门覆盖不替代原生交互实测。
- ACK 丢失、提交不确定、旧操作、断线、崩溃与重启恢复。
- 生产 Run 的启动/退出、认证与 profile、设备配对、绑定撤销、异步发送及持久化。
- 真实 Telegram 和模型、其他 CLI 版本以及 Linux/Windows。

渠道沿用同一 Thread 的上下文；模型为渠道生成的回复可能引用本机历史。来源门阻止自动转发本机发起的 Turn，不能承诺模型回答不会引用共享历史。

首片自动化结果不代表完整第一阶段通过。下一片先完成剩余原生边界及 P1 生产契约，按明确支持矩阵交付真实文本闭环和必要交互；跨会话管理继续独立确认。
