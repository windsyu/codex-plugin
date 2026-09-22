# R0 阶段证据：普通 CLI、模型代理与 Chrome 实时阅读

日期：2026-09-18。对应 [V2 实施计划 R0](../v2-implementation-plan.md)。本文记录已执行实验、支持范围和测量限制；切片状态只在实施计划维护。当前证据针对安装版 0.154.0 和下述必要 custom profile，不代表 R1–R5 产品验收。

## 1. 环境与范围

| 项目 | 本次实际值 |
| --- | --- |
| 工程起点 | `930172493c163a903621ca14c540d1ca10dd4f3e`，分支 `codex/native-cli-live-workbench`，未提交工作树 |
| 官方 CLI | `/opt/homebrew/bin/codex` → 安装目录 `0.154.0/bin/codex`；`codex-cli 0.154.0` |
| 官方研究源码 | `633ab199cfd724aa78013c006b27a2b3d049fc3b`，只读 |
| 网络 | 合成 loopback HTTP/SSE/WS；另完成 §2.4 的真实 HTTPS/SSE 单轮、多轮、工具和取消联合实验 |
| CLI 配置 | 均使用临时 `CODEX_HOME`；合成实验使用假凭证，真实实验只读复制当前 custom provider/model/推理设置至权限 0600 的临时配置 |
| 项目 | 临时目录，只有合成 README；原生信任选择仅针对该目录 |
| Web | R0 最小正文阅读页；Chrome `153.0.8010.50`，回归使用 headless，真实联合与 Paint 测量使用 headed；隔离浏览器目录，无额外下载 |
| 测量机器 / 构建 | Darwin arm64、Apple M3 Max、36GiB RAM；Rust dev/debug 构建，测量工具含额外观测开销 |

配置覆盖入口与[官方配置文档](https://developers.openai.com/es-419/docs/config-file/config-advanced)相符；本次结论来自下述安装版实验，文档入口本身不代表已验证实际 provider。测试中的模型名和认证结构不构成真实服务的兼容证明。

## 2. 已实现并运行的检查

新模块位于 [src/workbench](../../src/workbench/)，由独立 [lib.rs](../../src/lib.rs) 导出，不依赖旧 Controller、Session Runtime、数据库 writer 或 command ledger。

| 检查 | 断言与实际结果 | 证据位置 |
| --- | --- | --- |
| 固定上游 | 拒绝远程明文 HTTP、URL 凭证、query/fragment、路径逃逸；允许明确 loopback HTTP fixture | [proxy/tests.rs](../../src/workbench/proxy/tests.rs) |
| 双向流式 | 合成上游在请求尾片释放前收到首片；客户端在响应尾片释放前收到 SSE 首片；两端 UTF-8 字符跨片、CRLF、正文、认证和应用 header 保持 | 同上 |
| 传输语义 | gzip 响应字节不解压；429、Retry-After、307、Location 与响应正文保留；不跟随重定向，不新增重试 | 同上及 [proxy.rs](../../src/workbench/proxy.rs) |
| 原始 WS | 握手、掩码、分片、二进制、RSV1 压缩消息、ping/pong、close 字节在 tunnel 中保持一致 | 同上；仅为传输证明，未声称观察侧支持压缩 WS |
| 访问边界 | 错误 Host、浏览器 Origin/Sec-Fetch-Site、缺失路由能力、非法方法、CONNECT、absolute-form 和越界路径均未到达上游 | 同上 |
| 有界观察 | 字节和消息预算限制入队；消费者仍持有的观察副本继续计费；满队列和消费者关闭时计数缺口 | [capture.rs](../../src/workbench/capture.rs) |
| 观察停顿 | 消费者停顿完整 5 秒；512KiB 响应在其恢复前完成，受 3 秒客户端超时约束；队列计费不超过 1KiB，丢弃的是观察副本 | [proxy/tests.rs](../../src/workbench/proxy/tests.rs) |
| 连接与取消 | 64 个在途转发占满后返回 503；客户端断开释放容量；Run 退出和客户端取消释放上游响应；截断正文向客户端报错并记录 interrupted | 同上 |
| 结束状态 | 空请求、固定 Content-Length、正常 chunked 与 WS 结束不误报中断；结束标记无法入队时仍有独立中断/丢失计数 | 同上及 [capture.rs](../../src/workbench/capture.rs) |
| 传输统计 | 分别计数 HTTP 请求/响应、SSE 响应、WS 请求/响应；计数不保留 header、凭证或正文 | [capture.rs](../../src/workbench/capture.rs) |
| 增量解帧 | SSE 全切分位置覆盖中文、CRLF、BOM、多行 data；超限丢弃到明确边界，缺口不拼接成另一条合法事件；WS 覆盖掩码、分片、控制帧和扩展长度 | [framing/tests.rs](../../src/workbench/framing/tests.rs) |
| 解帧不支持 | WS 丢片后不猜帧边界；扩展帧明确返回 unsupported；不改动网络转发。解帧输出仍是未脱敏瞬时数据，不能直接送往浏览器或记录器 | [framing.rs](../../src/workbench/framing.rs) |
| 安全正文解码 | 已知凭证跨增量边界脱敏；不暴露原始 JSON、加密 reasoning、工具私有字段或未知事件正文。坏 JSON/丢片后抑制后续增量，完整最终文本可替换恢复；HTTP 请求与 WS 多响应身份分开 | [decode/tests.rs](../../src/workbench/decode/tests.rs)、[redaction.rs](../../src/workbench/redaction.rs) |
| 内存阅读与直推 | 快照与重放接续无注册窗口；事件带实际正文和 revision，最终全文替换去重；单 Item、Run、重放 ring 和阅读者队列有独立上限，过期 cursor/慢阅读者要求重新取快照 | [live/tests.rs](../../src/workbench/live/tests.rs) |
| Web 边界 | 独立配对能力、HttpOnly/SameSite Cookie、Host/Origin/Sec-Fetch-Site 拒绝、CSP 与无缓存；两个 Run 的 Cookie 名独立；最多两个在途快照正文，超限明确拒绝 | [web/tests.rs](../../src/workbench/web/tests.rs) |
| 调度与故障组合 | 观察器与 Web 各自独立执行线程；观察器暂停 5 秒时 512KiB 网络正文仍在 3 秒超时内完整收到，观察计费不超过 32KiB且缺口可见；恢复后坏 JSON 不阻止另一请求的 40 次正文增量，慢阅读者被独立关闭 | [observe.rs](../../src/workbench/observe.rs)、[web.rs](../../src/workbench/web.rs)、[reading.rs](../../src/workbench/proxy/tests/reading.rs) |

### 2.1 安装版普通 CLI 的单次响应接入

[native_cli.rs](../../src/workbench/proxy/tests/native_cli.rs) 显式运行安装版普通 `codex`，仅传本次 `-c model_providers.custom.base_url=…` 覆盖；没有启动本项目外部 App Server，也没有使用 `codex exec` 或 remote TUI。

已执行并通过：

1. 新建临时配置和合成项目，通过 PTY 完成原生目录信任。
2. 在原生输入区粘贴中文并 Enter；合成服务核对请求确实包含该中文、原模型名和合成认证。
3. 临时配置的原地址指向不可达端口，实际请求到达本次代理，证明进程级覆盖生效。
4. 合成服务发送首个正文 delta 后，等待独立系统 Chrome 页面实际出现中间正文，才允许发送后续文字和完成事件。断言此时上游尚未发送完成事件；该证据已从原始观察回调扩展至浏览器 DOM。
5. 原生终端显示助手的 `R0_NATIVE_OK`；用户输入中不包含该标记，避免把输入回显误认作助手回复。
6. 比较临时配置中的 provider、model 与 model_provider，均保持原值。CLI 原生写入的临时目录 trust 允许存在；真实用户配置未作为写入目标。
7. 终端回复可读后结束本次测试子进程、PTY、loopback fixture 并清理临时目录；不据此把模型 response 完成当作 Codex Turn 完成证据。

首次实验显示：输入过早会被原生启动输入边界清理，驱动必须在信任提示稳定后明确 Enter。实验没有通过预写 trusted 配置绕过原生页面，也没有给生产工作台增加自动 Enter。安装版还会发出独立的结构化任务标题请求，夹具按实际 schema 单独响应；不把它算成同一请求的重试，也不修改 CLI 去禁用该原生行为。主对话请求次数另有断言，后台异常不能被总体测试成功掩盖。

### 2.2 Chrome 的实际页面检查

[浏览器探针](../../web/e2e/r0-reading-probe.cjs)由 Rust [页面实验](../../src/workbench/web/tests/browser.rs)及上面的原生接入实验驱动，仅通过子进程环境传递本次配对入口，错误输出只保留检查阶段，不打印 capability。

- 首片可见后才释放最终响应；正常流式期间只有一次 snapshot GET 和一条 SSE 连接，结束事件不触发重取正文。
- 最终全文替换原有卡片，状态只表达“本次模型响应已结束，不代表任务结束”。
- 合成 HTML 注入字符串保持为文字，未生成图片、脚本或 iframe 节点；跨片的合成 secret 未进入正文。
- 刷新后通过 Cookie 恢复同一 Run 的已捕获内容，总计两次快照/两条 SSE；地址栏中的配对 fragment 已移除。
- 1024 / 736 / 320px 检查均无页面整体横向溢出；浏览器报告 0 个页面错误。
- 页面明确“保存未启用”，快照持久水位为 0，历史范围为部分正文。它没有终端输入器或产品级三列布局，不取代[已归档交互原型](../prototypes/README.md)。

这是安装版 Chrome 的 headless DOM/布局检查，不是前台显示器首次 paint 或 R1–R4 原型验收。截图可用下节环境变量输出到临时路径，不携带真实会话或凭证。

### 2.3 原生多轮、工具与 Esc 取消

[interaction.rs](../../src/workbench/proxy/tests/native_cli/interaction.rs)已执行：中文输入 → CLI 实际执行 `printf` 只读命令 → 下一次模型请求携带对应 `call_id` 的 `function_call_output` 及合成输出 → 原生显示第一轮回复 → 第二轮未完成响应 → 用户 Esc → 上游响应被取消 → 新输入仍正常完成。

工具成功断言来自后续请求中的真实工具返回，不来自模型生成的参数。取消使用没有总超时的生产代理客户端，测试在上游取消和原生中断提示均出现后才输入下一轮，不能由测试 HTTP 超时或最后清理进程来伪造通过。驱动以原生 Working/取消提示和已观察正文确定取消时机，不要求 TUI 的未完成行已经刷新。四次主模型请求的认证、模型和中文输入分别核对；原 provider/model 配置保持原值。

### 2.4 当前真实 profile 的单轮与多轮联合验证

[r0-live-profile.rs](../../examples/r0-live-profile.rs)默认只输出经过白名单处理的 profile 属性；显式 `--live` 才运行合成任务。已完成一次普通 CLI → 固定 HTTPS upstream → SSE 观察 → headed Chrome → 原生终端及临时 rollout 的联合验证：

- profile 为 `custom` / `gpt-6-astra` / `max` reasoning，沿用原配置 bearer；没有换模型、认证或推理强度，没有强制关闭 WS。原配置未声明 `supports_websockets`，本次实际观察到 2 个 HTTP 请求、2 个 SSE 响应、0 个 WS。
- CLI 完成隔离目录的原生信任后接收中文提示，只生成虚构月球图书馆导览。任务标题与主回复是两个独立请求；浏览器使用任务专用正文标记、最小字符数和 request ID 选中主回复。
- 探针开始后 **65.310 秒**，前台浏览器已有 163 个正文字符，且同一 response 在内存状态中仍为 `receiving`；**92.024 秒**模型响应结束，随后刷新恢复同一请求。
- **92.131 秒**完成核对：最终正文 1117 字符；原生 TUI 显示未出现在提示中的正文尾部；临时 rollout 的原生完成事件无错误，`last_agent_message` 与该正文完全一致。网页与原生完成事实分别验证。
- Chrome `153.0.8010.50`、`headless=false`、0 个页面错误；原配置字节以及临时 provider/model 保持不变；观察队列丢片/丢字节均为 0。
- 捕获仍为 `partial`：正文模式按策略省略其他内容，两个原始流记录 `interrupted`。模型/原生完成验证不抹掉 HTTP EOF 前关闭的事实，也不把此状态解释成观察队列丢字节。

早期探针曾把短任务标题当成主回复，之后在 90 秒截止前只读到 reasoning 项、没有主回复完成记录；这些尝试均未计为成功。当前探针保留原 `max` 设置，将观察窗口扩至 5 分钟并每 15 秒输出不含正文的进展。上面的时间含原生启动和模型生成，不是代理开销，也不是纯模型 TTFT。

随后运行同一探针的 `--live --interactions`，补齐真实模型的多轮/工具/取消，精简数值证据见 [JSONL](native-cli-r0-live-2026-09-18.jsonl)：

1. 原生 CLI 执行合成只读 `printf`。当前 profile 实际使用 Code Mode `custom_tool_call`；按相同 `call_id` 找到原生持久的 `custom_tool_call_output`，其 `input_text` 包含结构化 `exit_code=0` 和预期 stdout。工具成功不来自模型正文。
2. 第一轮 Chrome 在 **21.717 秒**已有 172 字符的中间态，**31.127 秒**响应结束；594 字符的最终正文与原生终端、rollout 完成记录一致。
3. 在同一个 PTY/CLI 提交第二轮，看到尚为 `receiving` 的主回复正文后发送 Esc；观察到新增原生 `turn_aborted` 及未完成 SSE 流关闭。取消证据发生在实验清理之前。
4. 同一 CLI 再接收第三轮中文输入。另一观察页面验证中间正文、结束与刷新；终端最终尾部和原生完成记录再次匹配。全流程 **97.968 秒**，5 个 HTTP 请求/5 个 SSE 响应/0 WS，观察丢片与丢字节为 0，原配置及 provider/model 保持不变。

最初两次工具探针只接受 `function_call_output`，虽然已看到主回复完成，仍因无法核对工具结果而返回失败。修正前先用合成服务和实际安装版 CLI 复现 Code Mode 的多段返回并核对退出码，增加相应测试，再进行上述成功实验；不把失败尝试计为通过。官方研究源码的 `ExecCommandEnd` 是非持久事件，工具核对不能假定它出现在 rollout 中。

这些结果证明本次隔离配置的原生行为；命名 profile、项目配置和显式 resume 的合成实验见 §2.6–2.7。其他认证、管理策略或原用户目录中全部配置组合不在本次真实模型结论内。

### 2.5 合成转发、DOM 和主帧 Paint 测量

[r0-latency.rs](../../examples/r0-latency.rs)与 [Chrome 测量探针](../../web/e2e/r0-latency-probe.cjs)驱动生产 Proxy、Observer、LiveHub、ReadingServer 和实际阅读页。完整数值报告见 [JSONL](native-cli-r0-latency-2026-09-18.jsonl)，只保存统计量，不保存能力令牌、页面内容或原始 Chrome trace。

每个案例先以固定 SSE 内容/频率直连，再经过代理；接收端为合成 HTTP 客户端，**不是普通 CLI**。直连与代理的“发出至客户端”包含各自本机调度，分位数不能当作逐片配对的因果差值。代理观察接收时间与客户端时间使用同一 monotonic 时钟；浏览器在流量前后各做 15 次 RTT 中点校准，取最小 RTT 样本。本轮半 RTT 误差估计 ≤0.167ms，前后 offset 差的绝对值 ≤0.063ms。

表中时延单位 ms，采用 nearest-rank 分位数；完整 p50/p95/p99/max、样本数均在 JSONL。

| 案例 | 客户端 / 正文观察样本 | 直连发出→客户端 p95 | 代理发出→客户端 p95 | 代理接收→客户端 p95 | 接收→DOM p95 | 接收→下一主帧 Paint p50 / p95 / p99 |
| --- | --- | --- | --- | --- | --- | --- |
| 单请求 100Hz、约 64B/片 | 400 / 400 | 0.321 | 0.462 | 0.113 | 1.258 | 5.481 / 10.407 / 11.178 |
| 单请求 100Hz、约 2KiB/片 | 400 / 400 | 0.270 | 0.494 | 0.124 | 33.834 | 34.574 / 74.092 / 93.288 |
| 4 请求并发、各 50Hz | 800 / 800 | 0.626 | 0.556 | 0.129 | 0.658 | 4.880 / 11.207 / 13.249 |
| 观察器停顿 5 秒、32KiB 观察预算 | 256 / 15 | 0.221 | 0.384 | 0.082（仅捕获的 15 片） | 4888.111 | 4870.093 / 4892.715 / 4892.715 |
| 页面主线程阻塞 1 秒 | 300 / 300 | 0.289 | 0.479 | 0.117 | 850.796 | 8.487 / 854.073 / 972.831 |

故障结果：

- 观察器停顿时，客户端 **1.232 秒**收完全部 256 个样本，早于观察器恢复；观察预算维持 32KiB，上报 261 个丢弃观察块、1,045,566 字节（含末尾完整文本事件）。恢复后仅统计确实捕获的 15 片，没有把丢失样本当作零时延。
- 慢页面可见时延明显增加，但客户端转发继续，300 个样本全部接收、观察无丢片。正常和慢页面案例均只请求一次 snapshot，未按 delta 重取全文。
- 全部案例页面均 `visible=true` 且有焦点、0 个页面错误；trace 的对应 mark 与主帧 Paint 均实际存在。Paint 测点是 DOM commit 后同一主帧的首个 `Paint`，不是显示器最终呈现时刻，也不证明并发时屏幕外的每条消息都被绘制。不能用此报告宣布全产品的端到端可见指标已通过。
- Rust 测量进程的生命周期峰值 RSS 约 **37.9MiB**，包含合成服务、代理、观察、Web 与测量代码；各代理阶段 CPU 累计 0.245–0.961 秒。Chrome 进程 RSS 之和峰值约 **1.06–1.57GiB**，包含独立浏览器、GPU/renderer、trace 与重复计入的共享页；不是产品独占内存。JS heap 峰值约 2.0–2.35MiB，浏览器 CPU 累计 0.677–5.560 秒。它们是带测量开销的采样，不是固定内存上限。

已有数据支持热路径隔离；普通 CLI 直连/代理对照补充见 §2.8。Web 发送阶段的独立测点、物理呈现边界及完整 Recorder/磁盘故障仍需随产品接入继续验收。

### 2.6 安装版的配置层验证

[native_cli.rs](../../src/workbench/proxy/tests/native_cli.rs)新增两个配置场景，均经过原生目录信任、真实请求检查及 Chrome 中间态/刷新：

| 配置场景 | 实际请求与保存检查 |
| --- | --- |
| 基础 `config.toml` + `--profile r0` 选择 `r0.config.toml` + 本次 `-c model_providers.custom.base_url` | 请求到达本次代理，使用 profile 的合成认证和原模型；基础/profile 中的原地址与认证均未改写 |
| 上述层级 + 原生信任后的项目 `.codex/config.toml` | 项目的 `model_reasoning_effort=low` 出现在实际请求中；项目内试图覆盖 provider 地址/认证的设置未生效，请求继续使用 profile 的认证与本次代理路由；项目配置字节不变 |

研究源码 `633ab199cfd724aa78013c006b27a2b3d049fc3b` 的 `codex-rs/config/src/loader/mod.rs` 中，`PROJECT_LOCAL_CONFIG_DENYLIST` 明确排除项目层的 `model_provider`、`model_providers`、`openai_base_url` 等字段；实测与此一致。不能只按“项目层高于用户层”复制一个通用 TOML 合并器，否则会错误选择认证或上游。

测试使用含 `name`、`wire_api` 和认证方式的完整合成 provider 定义；缺少必填 provider 名称会使原生信任保存时的配置校验失败，此前的不完整夹具已修正。没有通过改 CLI 或绕过 trust 来使测试通过。

官方 loader 将 legacy managed 文件/MDM 层置于运行参数之上，不能宣称 `-c` 总是最高优先级。[config_sources.rs](../../src/workbench/config_sources.rs)固化了当前 unmanaged 路径的只读检查，在代理和 CLI 启动前运行：

- 本机 `/etc/codex/{config.toml,managed_config.toml,requirements.toml}` 和 `com.openai.codex` 的 `config_toml_base64`、`requirements_toml_base64` 均未发现；只查询存在性，不读取或输出策略值。
- 当前原生认证元数据是 API-key 模式，没有 ChatGPT tokens。File 是该源码基线的默认认证存储；keyring、auto、ephemeral、未知认证模式及可能的云管理来源均拒绝进入当前捕获路径。
- 两项合成负向测试覆盖系统文件、MDM 存在标记、认证存储、ChatGPT/未知模式和损坏元数据；策略文件字节不变，错误不包含原凭证正文。其他 OS 的配置来源检查尚未验证，会明确拒绝。
- 默认 `r0-live-profile` 只输出上述布尔量及已允许的 profile 属性，确认原配置字节不变。检查不请求真实模型、不修改 `/etc`、MDM、keyring 或用户配置。

这是一条有明确拒绝边界的必要 profile 验证，不是通用官方配置合并器。真实探针仍将明确的选定设置复制到隔离 home；R1 Launcher 必须复用此边界、保持用户 home 和原生配置流程，不能将命名 profile 的合成通过自动扩大为生产支持。

### 2.7 显式原生 resume

[resume.rs](../../src/workbench/proxy/tests/native_cli/resume.rs)已执行：普通 CLI 完成合成首轮并留下原生 durable 完成记录 → 原生 Ctrl-D 正常退出 → 新建代理与路由能力 → 用户显式 `codex resume <原 thread UUID>`。

恢复后的终端显示原历史；保持空闲 400ms，没有新增主模型请求。仅在提交新中文输入后发出下一请求，包含原提示/回复上下文且经过新代理，原 thread 增加新的 durable 完成记录。两次 CLI 都正常退出，原 provider/model 配置保留，观察丢失为 0。测试不使用 `--last` 猜会话，也不自动重放前次输入。此结果不代表服务可恢复已退出的 PTY，生产网页重连仍由 R1 验收。

### 2.8 普通 CLI 的固定流直连/代理对照

[r0-native-latency.rs](../../examples/r0-native-latency.rs)在同一安装版、120×45 PTY 和合成配置上比较普通 CLI 直连与新代理。每个案例运行两轮，顺序为直连→代理、代理→直连；共 12 次独立 CLI 运行，结果见 [JSONL](native-cli-r0-native-latency-2026-09-18.jsonl)。每次均经过原生信任和中文输入，只有一次主模型请求，另保留 CLI 自有标题请求。

上游以固定 100Hz 发送带编号的中文正文行。起点是 fixture 将 SSE 片段交给 HTTP body，终点是实验读取到能在 VT 屏幕中确认该编号的 PTY 输出；两者使用同一进程 monotonic 时钟。PTY read 时间在独立读线程记录，避免把测量循环排队当作读取时刻。该测点包含 CLI 缓冲和渲染，不是 CLI socket 接收或物理显示时间。

| 案例 | 每次采样 / 正文片段字节 | 直连发出→PTY p95，两轮 ms | 代理发出→PTY p95，两轮 ms | 代理 Enter→原生完成，两轮 ms |
| --- | --- | --- | --- | --- |
| 短文本 | 160 / 60B | 18.499 / 20.145 | 20.710 / 21.312 | 1696.286 / 1699.298 |
| 长输出 | 120 / 1568B | 10.022 / 10.422 | 10.100 / 10.176 | 1390.632 / 1388.550 |
| 观察器暂停 5 秒，32KiB 预算 | 120 / 320B | 99.273 / 100.161 | 101.331 / 100.394 | 1306.744 / 1304.542 |

全部 **1600 个 PTY 标记**均被观察到；每次终端都在上游结束前出现中间内容，最终完整正文与原生 rollout 完成记录严格一致，provider/model 保持原值。正常代理案例观察丢失为 0。两次暂停案例分别在暂停开始后 **1442.063 / 1444.678ms** 完成原生任务，早于 5 秒观察恢复；各报告 127 个丢弃观察块、156295 字节，32KiB 预算和恢复后的 partial 状态均有断言。

短文本/长输出的分位数在两种路由间相近；独立运行的 p95 差值不是逐片配对的因果开销，不能用更低的代理 p95 宣称代理加速。结合 §2.5 同时钟的代理接收→合成客户端 p95 ≤0.129ms、正常接收→Chrome 主帧 Paint p95 ≤74.092ms，这些数据支持 R0 的初始隔离和性能目标；完整产品与故障态并不承诺相同可见时延。

早期测量夹具遗漏 `response.output_item.added`，导致原生终端只在最终全文时显示消息；该轮不纳入表中。按官方 `core/tests/common/responses.rs` 与流式 Item 测试补齐消息开始事件，并增加“首个 PTY 标记必须早于末片发出”的断言后，才执行上述 12 次结果。没有调整或修补官方 CLI。

## 3. 复现命令与结果

在仓库根目录执行。首次依赖补齐需访问公开 Cargo registry；后续已使用离线构建。安装版探针需要本机 PTY 与系统 Chrome 权限，默认测试集将六个实机实验标为需要显式执行。

```sh
npm run build --prefix web
cargo test --offline --lib workbench
cargo test --offline --lib installed_cli_uses_per_process_route -- --ignored --nocapture
cargo test --locked --offline --lib installed_cli_multiturn_tool_execution -- --ignored --nocapture
cargo test --locked --offline --lib installed_cli_code_mode_tool_return -- --ignored --nocapture
cargo test --locked --offline --lib installed_cli_route_override_preserves_profile -- --ignored --nocapture
cargo test --locked --offline --lib installed_cli_explicit_resume -- --ignored --nocapture
WORKBENCH_TEST_SCREENSHOT=/private/tmp/native-cli-r0-reading.png cargo test --locked --offline --lib system_chrome_observes_safe_midstream -- --ignored --nocapture
cargo test --locked --offline --lib workbench -- --include-ignored
cargo test --locked --offline --example r0-live-profile --example r0-latency --example r0-native-latency
cargo run --locked --offline --example r0-live-profile
WORKBENCH_PROBE_HEADED=1 cargo run --locked --offline --example r0-live-profile -- --live
WORKBENCH_PROBE_HEADED=1 cargo run --locked --offline --example r0-live-profile -- --live --interactions
WORKBENCH_PROBE_HEADED=1 cargo run --locked --offline --example r0-latency -- --case all
cargo run --locked --offline --example r0-native-latency -- --case all --rounds 2
rustfmt --edition 2024 --check src/lib.rs examples/r0-live-profile.rs examples/r0-latency.rs examples/r0-native-latency.rs
cargo clippy --locked --offline --lib --tests --examples -- -D warnings
node --check src/workbench/reading/app.js
node --check web/e2e/r0-reading-probe.cjs
node --check web/e2e/r0-latency-probe.cjs
```

最新锁定依赖合跑：**51 项全部通过、0 ignored，用时 6.99 秒**，包含六个要求实机环境的显式测试。测量分位数与原生探针的光标、文本比对、工具结果判定另有 **5 项 example 单测通过**。合跑曾暴露启动驱动过早发送 Enter 的问题；驱动现确认原生输入区已显示草稿并经过 100ms 粘贴稳定期，再提交一次，按实际 VT 光标位置响应终端查询。Rust 格式检查、Clippy（含 examples）、JavaScript 语法检查及相关文档的本地链接/锚点和代码围栏检查通过；既有前端构建已在前一阶段通过，本阶段页面资产由 Rust 独立嵌入。默认集中的 ignored 表示环境要求，不能把已显式执行的实机实验记为未执行。`--live` 会向原配置的真实模型发送合成任务，其余命令没有真实模型调用。

## 4. 支持边界与后续验收

- 当前 unmanaged custom/max profile 已验证真实 bearer、HTTPS/SSE、主回复中间态、刷新、原生完成和配置字节不变；命名 profile、允许的项目设置和项目 provider 禁止覆盖已有合成证据。生产 Launcher 尚未交付，R1 必须复用已验证的配置来源拒绝边界。
- 安装版合成 SSE 与当前真实 custom/max profile 都已覆盖多轮、工具、原生 Esc 和下一轮输入，显式 resume 已用原生 durable history 验证。其他真实 WS provider、ChatGPT/keyring/managed profile 仍为 unverified，不默认启用，也不为兼容而降级原生传输。
- 当前 decoder/网页只展示模型正文和响应状态，不包含产品级工具、请求上下文、Markdown、终端输入或历史功能。WS 文本缺少可确认身份时显式省略，不猜当前 response。
- gzip 与压缩 WS 已验证转发；观察侧解压尚未实现。无法解码时须明确 capture unavailable，不能为了观察而更改原生传输。
- 5 秒停顿已扩展至实际观察线程，慢阅读者、坏 JSON 和后续正常请求已联合验证；完整 Recorder/磁盘故障尚未接入，不能将内存实验算成持久化验收。
- §2.5 已测合成 HTTP 客户端转发、DOM/Paint、时钟映射、RSS 和 CPU；§2.8 补齐普通 CLI 对照与暂停隔离。Web 发送阶段和完整 Recorder/产品资源测点仍待后续切片补齐，不能声明全部产品性能门槛已通过。
- R1–R5 产品功能与交互原型的实机对照未开始验收。

下一步 R1：正式目录 Launcher、受认证的终端 WS、内存单写接管、持续挂载的三列原型界面和同进程刷新。旧服务、数据库 schema、`/v1` 和用户全局配置未切换，未执行 migration、提交或远程变更。当前分支仍为 `codex/native-cli-live-workbench`，代码与文档是未提交工作树。
