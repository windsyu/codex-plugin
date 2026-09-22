# 原生 CLI 实时工作台详细设计

> 日期：2026-09-18；按用户完全重构授权改写。进度与逐片验收统一见 [V2 实施计划](v2-implementation-plan.md)。
> 上位方案：[工作台方案](codex-native-cli-workbench.md)；决策：[ADR 0039](decisions/0039-cc-viewer-style-runtime.md)。
> 本文包含目标契约与已验证的实现说明；只有明确链接验收证据的部分才表示已接入，完整 V2 仍未交付。

前端视觉与交互基准见[方案 §1.1–1.4](codex-native-cli-workbench.md#ui-prototype)及[交互原型](prototypes/native-cli-workbench.html)。原型使用合成数据；聊天角色、内容类型、原生终端生命周期、数据来源与异常状态仍以下述契约为准。

## 1. 运行单元与模块

一个 Run 对应一次目录启动，一个官方交互 CLI、一套 PTY、一组模型连接和一份观察记录。Run 不是 Codex Thread：原生 resume/fork、子代理和多轮任务可关联多个 Thread/Response。新服务不启动外部 `codex app-server`，不发 `turn/start`，不管理 Codex 的原生交互协议。

| 模块（拟定） | 职责 | 不依赖 |
| --- | --- | --- |
| Launcher | cwd、安装版检查、provider profile、启动/退出与浏览器入口 | 旧 SourceSupervisor/SessionIntent/DbWriter |
| PtyHost | 官方 CLI 进程组、输入、resize、输出和屏幕恢复 | ThreadLease/TurnOwner/command ledger |
| ModelProxy | 固定 upstream 的 HTTP/SSE/WS 转发、轻量 tee | SQLite、展示 reducer、网页 ACK |
| CaptureDecoder | 观察副本解码、边界/身份/缺口与脱敏 | 终端 ANSI、模型调度 |
| LiveStore + LiveHub | 内存状态、revision、快照、带内容的推送和短重放 | 磁盘写入成功 |
| Recorder + Indexer | 异步 journal、快照、可重建 SQLite 历史查询 | 转发和输入许可 |
| RolloutReader | 后台读取已确认关联的 Codex 历史，补足用户消息和本机工具结果 | Web 请求内扫描 store |
| WorkspaceReader | cwd 文件、rg、Git 阅读 | 任意 shell 或旧 Gateway command |

新代码可放独立 `src/workbench/` 和 `web/src/workbench/`。Rust/Tokio、Preact/xterm、安全渲染组件可保留；这是技术选择，不是复用旧控制契约。旧 `src/session/`、Controller 与 App Server 启动链已在 R5 退役，只有原生工作台可启动交互 CLI；兼容 Observer 只提供历史读取与显式维护。

2026-09-21 已将通用 VT 快照/过滤工具移至 `src/terminal/`，主题、终端配置和写入协调器移至 `web/src/terminal/`。新工作台不再通过旧 Session 目录引用它们；旧入口暂时引用同一份工具，迁移不改变终端行为。依赖检查、双入口测试及退役前仍缺少的验收见[终端工具独立化记录](validation/native-cli-r5-terminal-extraction-2026-09-21.md)。

## 2. 启动与 provider 配置

### 2.1 入口与生命周期

```text
cd <project>
codex-view
codex-view --no-open
codex-view --data-dir <private-history-directory>
codex-view --provider-profile unmanaged-custom
codex-view --profile <native-profile> --no-open
codex-view --codex-bin <installed-cli> --resume <explicit-thread-uuid>
codex-view open <private-entry-file>
```

命令已在独立 [binary](../src/bin/codex-view.rs) 和 [Launcher](../src/workbench/launch.rs)实现，[正式入口验收](validation/native-cli-r1-launcher-2026-09-19.md)覆盖合成模型、本机 Chrome 和安装版 CLI。Launcher 固定 canonical cwd/home、检查安装版，创建随机 `runEpoch`，先保留 loopback Web 端口并绑定模型代理，再启动普通官方 CLI 的 PTY。R3 在 `~/.codex-web/history` 建立按 runEpoch 分隔的观察历史，可用 `--data-dir` 指定独立目录；项目身份使用 canonical cwd 的摘要。它不是原生 Thread 身份或可恢复的 CLI 控制状态。记录器异步建目录；不可写时运行继续并提示保存失败。

R3.1 已接入独立工作台 JSON 配置与 `--config-dir`，现有显式参数覆盖文件值；上述命令与数据默认值继续兼容。具体目录、字段与下次启动生效规则见[配置契约](codex-native-cli-workbench-history-settings.md#2-配置目录与文件契约)；增量验证与后续清理任务见实施计划。

生命周期分别处理：关闭网页仅断开 UI；原生 CLI 退出或 `POST /workbench/v1/run/stop` 停止本次子进程后，终端显示已结束，内存阅读与配对入口继续可用；启动器收到 SIGINT/SIGTERM/SIGHUP、装配失败或关键运行线程意外退出才清理其启动的 CLI 进程组、监听和私有入口。SIGHUP 覆盖启动终端挂断；不将仅关闭网页解释为挂断。stop 返回接受后异步结束 CLI，重复调用不重启。PTY 对仍存活的自有进程组发 TERM，约 750ms 后仍未结束则 KILL 并回收，不杀其他 Codex。原生结束后不启动 Shell。首版不做 daemon 脱离和 OS 终端/网页同时可写。SIGKILL 无法捕获，不能承诺执行相同清理；原生工具自行脱离该进程组的完整回收也不由当前测试证明。[R5 退出增量](validation/native-cli-r5-process-cleanup-2026-09-21.md)记录了实际信号回归及测试浏览器回收范围。

默认私有入口为系统临时目录下的 `codex-view-<epoch>/entry.json`（目录 0700、文件 0600），也可用 `--entry-file` 指定尚不存在的文件。配对 capability 仅写此文件，经系统浏览器打开；普通输出为安全地址、epoch、CLI PID、`cliVersion` 与文件位置。`open` 只接受当前 OS 用户拥有、无组/其他权限的普通文件，拒绝符号链接、超限文件和非 loopback/非法配对 URL。清理仅删除仍匹配原 inode/device 的自有文件及空的自有目录；异常被杀后的残留文件不是持久历史或服务存活证据。

按 [ADR 0045](decisions/0045-cli-version-compatibility.md)，版本探测只确认指定程序能在 5 秒内成功返回有界、可识别的 `codex-cli <version>`，不做精确版本、最低版本或 major 版本准入。输出最多 4096 字节，版本标识最多 64 个 ASCII 字符，以数字开头，仅允许字母、数字、`.`、`+`、`-`；不会把任意子进程输出写进诊断。未列入回归基线的版本打印一次提示后照常启动，不需要 bypass 参数。探测执行失败、超时、格式错误或实际配置/路由检查失败仍明确报错。未来 CLI 参数或协议真正变化时，应针对具体故障诊断，不能把版本号不同当作已证实不兼容。

目录就是 workspace 根。网页打开目录不改变 CLI cwd，原生导航也不自动扩大文件读取根。需要其他目录时新建一次运行。浏览器刷新、切面板或重新获得输入权不重新 spawn。

### 2.2 Profile 契约与 R0 门槛

```ts
interface CaptureProfile {
  profileId: string;
  validatedCliVersions: string[]; // 验证证据元数据，不作为启动准入
  providerId: string;           // 保持 CLI 实际 provider 身份
  authKind: "api-key" | "chatgpt" | "custom";
  upstreamBaseUrl: string;      // 本地受保护配置，不来自浏览器请求
  pathMappingVersion: number;
  transports: ("http-sse" | "responses-ws")[];
  overrideKind: "openai_base_url" | "custom-provider-base-url";
}
```

这里只记录已确认的模型路由，不复制 credential、不重写官方配置合并器。R0 必须找到并验证 profile 与 CLI 最终生效 provider/config 的核对方法，覆盖命令参数/profile/管理配置的优先级；无法证明时拒绝该 profile，要求显式配置原 upstream，不猜默认地址。[安装版实验](validation/native-cli-r0-progress-2026-09-18.md#26-安装版的配置层验证)已覆盖命名 profile、本次路由覆盖和项目层：项目允许的推理设置生效，但 `model_provider`/`model_providers` 等路由认证字段被原生过滤。当前必要 unmanaged custom profile 增加了启动前来源检查：检测到系统/管理层、非 File 认证存储或可能的云管理时拒绝；只输出存在性，不覆盖策略。R1 Launcher 必须复用该边界并保持原 home，不能用通用 TOML 合并推导 provider，也不能将合成配置实验扩为全 profile 支持。

已实现的适配器为 macOS 的 `unmanaged-custom`，CLI `0.154.0` 是初始基线，`0.155.1` 的[升级回归](validation/native-cli-version-compatibility-2026-09-20.md)覆盖正式入口与合成模型交互；这些版本不是启动白名单。适配器只从原 `config.toml` 与可选 `<name>.config.toml` 选择受测的标量路由字段，再把原 home/profile 交给 CLI。要求 provider `custom`、`wire_api=responses`、`requires_openai_auth=false`、显式 base URL 与非空静态 `experimental_bearer_token`；不覆盖模型、推理、权限或重试。`env_key`、`env_http_headers`、命令认证、AWS、managed/cloud/非 File 存储及 `supports_websockets=true` 均须另行验证，当前明确拒绝，不强制降级。配置文件最多 2MiB、认证元数据最多 1MiB，按 nofollow/nonblock 普通文件读取；启动前复查已读配置未改变，这不声称锁住所有配置源或消除并发编辑竞态。

项目路由字段仍由原生过滤，允许的项目推理设置在实机请求中验证；项目层的认证存储覆盖尚未验证则拒绝。配置不会由启动器复制或写回；CLI 自身的信任/设置写入照常，命名 profile 中可能新增 projects/trust。正式 binary 的普通启动、命名 profile、两轮聊天及退出已实测；[显式 resume 验收](validation/native-cli-r1-native-interactions-2026-09-19.md#4-正式-binary-显式-resume)另验证新 Run 复用明确原生 Thread、终端恢复旧历史、新输入前不请求模型、不重放输入。实时阅读仍只展示本 Run；R3 的“历史记录”另读取本项目已保存的运行，选择历史不会触发原生 resume。

只为本次子进程使用版本支持的覆盖参数/环境。保留真实用户 `CODEX_HOME`、原模型、权限、sandbox、OAuth 刷新和重试。自动化测试使用临时 home。API key 与 ChatGPT 登录的 URL、路径、WS upgrade 和辅助路由各有 fixture；保留 URL 后缀对官方 capability 判定的影响。安装版与研究源码不一致时以实测支持矩阵为准。

2026-09-19 按用户要求确定显示策略：正式 launcher 与 R1 调试入口均设置 `TERM=xterm-256color`、`COLORTERM=truecolor`，清除 CLI 子进程继承的 `NO_COLOR`，并追加 `-c tui.animations=false`。`NO_COLOR=1` 曾来自调试启动环境，使原生色彩检测降级；这不是 ANSI 转发过滤造成的。关闭动画使用[官方配置项](https://learn.chatgpt.com/docs/config-file/config-reference)，涵盖欢迎页、微光和旋转提示，工作台不通过匹配字符图案或删帧实现。只影响新启动的本次 CLI；不写回配置，不改模型/权限/输入行为。安装版与 Chrome 证据见[显示验证](validation/native-cli-terminal-display-2026-09-19.md)。

模型代理是显式 base URL 路由，不安装根证书、不劫持系统 TLS。上游仍验证 TLS。代理只转发 CLI 提供的认证，不成为 token 刷新器；不把 header/body 认证材料存入浏览器或 journal。URL 映射、重定向与跨 origin 转发不允许把凭证带往另一个域名。

浏览器认证与模型代理分离。Web 使用运行期配对 Cookie、严格 Host/Origin 和 CSRF 防护；模型代理使用本次运行的不可猜路由能力，固定 upstream，拒绝浏览器 Origin/CORS、CONNECT 和任意转发目标。路由能力不进入日志或页面，可经 base URL 前缀注入，出站前剥离并保留原上游路径；它是本机临时能力，不替代模型 Authorization。若该版本支持安全的独立环境 header，可在同一契约下替代路由能力。不能静默降低为开放代理。首版信任同 OS 用户，不声称能抵御该用户读取本次进程参数/内存。

上述为当前本机入口。2026-09-21 用户要求的 IP/MagicDNS 与扫码配对见[接入契约](codex-native-cli-workbench-device-access.md)及 [ADR 0051](decisions/0051-workbench-device-access.md)：一个共用设备监听复用同一 Router/WebState/Run，IP 与 MagicDNS 同时访问该端口；没有互斥模式或强制 Serve。前端相对 API/WS 保持，后端用已知地址集合及本次 Host/Origin 对应校验替换固定来源。固定配对码与授权共用，撤销贯穿 WS/SSE、终端排队操作与重连资格，模型代理不开放。已落在 web/access、共享 permission 与 AccessPanel；可选 access.port 只影响下次开启，状态与凭证不保存。自动化和 Chrome 结果见[接入验证](validation/native-cli-device-access-2026-09-21.md)，真机仍按 R5/P14 单独验收。

R0 先验证当前必要 profile：配置不污染、实际认证、CLI 会用到的 HTTP/SSE 或 WS、首轮/多轮/工具/取消、原生初始化与恢复；另用合成服务覆盖 SSE/WS 传输契约。其他 profile 通过同等实机验收后再加入支持清单，未测 provider 标为 unverified，不能默认启用。WS 转发未通过时不能强制 HTTP 以制造成功。

## 3. 转发与观察分离

### 3.1 热路径

以下是调度契约，不是要求忽略网络背压：

```text
网络片段到达
  → 分配该 connection 的接收序号
  → 送入有界转发通道，由独立任务立即写向 CLI/上游
  → try_send 观察引用；满则记 capture gap，不等待
观察任务
  → 解帧/解压副本 → 脱敏 → reducer → LiveHub
  → try_send Recorder；满则切换 recorder degraded
```

转发优先且不执行完整 JSON 解析、正文拼接、哈希大 blob、fsync 或数据库操作。HTTP request body 和 response body 均保持流式；观察不要求先读完整请求才发上游。只按 HTTP 语义改写必要的 authority/Host、hop-by-hop 与传输 framing，不改应用 payload、model、stream 选择、工具参数、状态码或原生重试。

支持 SSE 的跨 chunk UTF-8、CRLF、多行 data 和长事件；gzip/zstd 等只在观察副本解压且有限额。解码不支持时转发照常并提示 capture unavailable。WS 保留消息类型、顺序、控制帧及关闭语义；一个连接可承载多次 response create，不能把 socket ID 当任务 ID。代理不新增模型重试，不 replay 结果不明的请求。

观察缺口通过独立原子标记/有界区间汇总传达，不能依赖已经满的队列再塞一条 gap。后续 decoder 看到缺口须清理不完整解帧状态，并仅在可确认边界恢复；该响应标为 partial。JSON 事件内丢字节不能继续拼成貌似合法的另一条消息。

### 3.2 初始资源预算

以下为待 R0 压测调整的初值：转发每连接 1MiB 有界 buffer，观察队列每 Run 8MiB，重放 ring 16MiB，活跃正文单 Item 1MiB、Run 展示缓存 32MiB，单客户端待发 2MiB。连接/活跃请求数量另设上限。达到网络连接上限明确拒绝；不能接受后丢转发字节。

观察超限丢的是副本；大正文保留有界预览并标 `truncated`，完整内容只有确实已记录时才给详情入口。慢网页断开后重新快照；不能无限复制请求上下文。Recorder 使用独立线程/执行池，磁盘卡顿不可占住转发任务。CPU 密集解码、脱敏与渲染同样必须有界，不能用“异步函数”当作已隔离性能的证明。

## 4. 身份、事件和展示语义

### 4.1 身份

2026-09-20 起，实时快照与 `item.replace` 共用 `schemaVersion=2` 的 `ViewItem`。运行身份由 envelope 的 `runEpoch` 提供；本 Run 内的 `itemKey` 是稳定呈现身份，不能当作原生调用 ID。当前封闭来源契约如下：

```ts
type Evidence =
  | { origin: "model"; key: TextKey; captureSeq: number }
  | { origin: "rollout"; key: UserKey; source: UserSource }
  | { origin: "diagnostic"; requestId: string; captureSeq: number };
interface TextKey {
  requestId: string;
  responseId: string | null;
  wireItemId: string;        // decoder 稳定键；可能是缺 wire ID 时的显式 output slot 别名
  contentIndex: number;
}
interface UserKey { codexThreadId: string; codexTurnId: string; nativeItemId: string }
interface UserSource { sourceRef: string; byteOffset: number; ordinal: number | null }
```

模型与工具的 `itemKey` 分别以 model/tool 区分，内部使用固定字段顺序的 TextKey JSON；用户键使用 user 加原生 thread/turn/item，诊断键使用 notice 加 request/code。前端把键视为 opaque，不解析键猜角色。网络项目与原生用户项目的来源都不靠 wall clock 去重；rollout 没有网络关联时不编造 request ID，sourceRef 不输出原始本机路径。工具结果另在 `result.source` 保留实际请求或原生位置证据。

HTTP 请求分别编号；WS 每次 create 的 metadata、response 身份独立保留，尚未证明 create→response 关系时不按先后绑定。相同 response ID 的不同 HTTP attempt 仍因 requestId 不同而独立。正式 wire item ID 迟到时，仅显式 output slot / item 别名关系可保留原呈现键；冲突不按文本或 ID 前缀合并。

模型 Item 与原生 Item 不保证同名。HTTP metadata、受测 WS 关系或 rollout 的明确身份才允许关联 Codex Thread/Turn。当前用户提取依赖 §4.4.1 的规范 UserMessage；不从 role:user 请求上下文提取人工提交。compact、fork、续传或关系不明时展示请求上下文，不按文本/时间强去重，不虚构完整对话。

### 4.2 Reducer

| 捕获事实 | 更新规则 |
| --- | --- |
| 请求开始 | 展示请求卡/当前可见输入上下文；表示已观察发送，不等于服务已接受 |
| `response.created` | 绑定 response 身份与当前请求，创建运行态 |
| `response.output_text.delta` | 按 response/item/content index 追加正文；无身份则保留请求级事件 |
| 工具参数 delta / Item added | 显示“调用生成中/已生成”，不标为执行中或成功 |
| Item done / 最终完整内容 | 替换对应 Item 已累积正文，不把全文重复追加；缺失字段不擦除已有内容 |
| `response.completed/failed/incomplete` | 结束这次模型响应并记录 usage；不结束整个 Codex Turn |
| 响应传输 EOF / 中途断开 | 对明确观察到 created、但未收到终态的响应发布 incomplete，保留已有正文；同一 WS 中已收到终态的响应不降级，不据此断言工具取消或整个 Turn 完成 |
| 请求里的 function/custom tool output | 按明确 call ID 关联结果；不能当作新用户输入 |
| rollout 已知工具完成、diff、Turn 状态 | 以身份关联增补事实；冲突保留 provenance，不能覆盖较新的模型片段 |
| unknown / 捕获缺口 | 脱敏诊断与 partial；不阻止其他请求 |

模型提出的 patch 与实际磁盘/Git Diff 分开标示。没有执行证据时不得显示“修改成功”。不解读终端光标/文字来生成结构化审批、线程焦点或完成状态。仅显示公开可见 reasoning summary；加密 reasoning、认证、图片/base64 等默认省略并注明策略。

脱敏在进入 LiveStore、浏览器和 Recorder 前完成；转发给 CLI/模型的原 payload 不被脱敏修改。内存原始观察副本只在有界队列/decoder 内短暂存在，不输出到通用日志。

<a id="chat-item-contract"></a>

### 4.3 聊天角色与类型契约

按 [ADR 0043](decisions/0043-unified-reading-items.md)，2026-09-20 已将用户、模型正文、工具和安全提示收敛到统一 DTO；[契约实现](../src/workbench/live/view.rs)、[前端封闭类型](../web/src/workbench/viewItems.ts)和[验证记录](validation/native-cli-r2-view-items-2026-09-20.md)对应本节。后端的来源聚合仍分类型维护各自预算，但快照只有一个 `items` 数组，浏览器也只保存这一份阅读项目；渲染选择器没有第二份长期状态。该变更不涉及旧数据库 migration。

```ts
interface ViewBase {
  itemKey: string;
  revision: number;
  orderIndex: number;          // 首次观察顺序，不冒充原生事件全序
  completeness: "observed" | "partial" | "omitted";
  truncated: boolean;
}
type TextContent = { contentKey: string; text: string };
type MessageItem = ViewBase & {
  kind: "message";
  author:
    | { role: "user" }
    | { role: "assistant"; requestedModel: string | null; reportedModels: string[] };
  content: TextContent[];
  streamState: "receiving" | "ended" | "incomplete";
  evidence: Evidence[];
  omitted?: boolean;          // user 必有；非文本省略与文本截断分别表达
};
type ToolCallItem = ViewBase & ToolCall & {
  kind: "tool_call";          // ToolCall 的完整字段见 §4.5.1
  evidence: Evidence[];       // 生成来源；执行证据另在 result.source
};
type NoticeItem = ViewBase & {
  kind: "notice";
  code: "unassigned" | "unknown_type" | "capture_gap" | "omitted";
  text: string;               // 固定安全说明，不串入未知 payload
  evidence: Evidence[];
};
type ViewItem = MessageItem | ToolCallItem | NoticeItem;
```

当前每条模型消息对应一个 TextKey/content index，contentKey 为该 index 字符串；用户文本 contentKey 为 text。不会将无顺序证据的不同片段合并成一段正文。消息模型来源或正文终态变化会增加同一 item 的 revision 并发送 replace。文本已收到权威全文时为 ended；尚未完成的正文遇到响应失败/不完整时为 incomplete；response completed 仅结束对应正文，不证明工具执行或整个 Turn 完成。

模型/工具 `evidence` 含一条 model 观察来源，用户含一条 rollout 来源，后端 notice 含一条 diagnostic 来源。字段只表示实际已捕获事实，不填充不存在的 connection/stream/native ID。工具生成/结果不完整或身份冲突，以及消息截断/不完整，均标 partial；省略与截断提示继续保留。浏览器遇到未来未知 kind/author，在有合法稳定 envelope 时转为固定 notice，丢弃未知 payload；非法身份或 patch 不按猜测应用。

`MessageItem` 的内部 `assistant` 角色在页面明确显示为“模型”，不得只有固定的“Codex”标志。模型名取 author 中的已确认 requestedModel/reportedModels；只有请求值时注明“请求模型”，响应报告值有差异时保留两者来源；未知显示“名称未确认”，不得采用静态原型模型名。用户气泡标“用户”，不凭连接输入权猜具体发送者。`streamState=ended` 只表示这条消息正文结束，不表示工具或整个任务完成。

请求用途由版本受测的 metadata/adapter 结构或明确原生历史事实确认，并保留分类依据。标题、摘要等辅助请求进入请求列表，不混入聊天正文；不能因为文本短、JSON 含 `title` 或看到 `role:user` 就断定用途/人工来源。用途或作者来源不明时保留请求级内容与“来源未确认”提示；R1 尚无完整请求面板时使用只读折叠上下文，不能把它冒充用户消息。

工具类别通过已识别的工具定义和参数 schema 映射：结构化 shell 调用才能显示命令卡；Code Mode `custom_tool_call` 的 `exec` 默认显示代码工具卡。不得通过正则扫描 JavaScript 内的 `tools.exec_command(...)` 生成声称已经执行的子命令。普通模型 Markdown 中的 shell 代码块仍属于模型文字。命令预览仅表示所请求的命令；有执行证据后才显示执行状态。未识别工具名可用 `category=other` 保留安全参数预览；未知 wire 类型走 notice。

R2 请求上下文适配须覆盖两种受测形态：标准请求顶层 `tools`，以及 Responses Lite 在 `input[]` 内的 `type=additional_tools`、`role=developer`、`tools`。后者可含 `type=namespace` 的分组；保留定义所在位置、namespace 与名称，不能按短名称跨 namespace 合并，也不能把开发者工具定义当用户气泡。安装版 0.154.0 的 `gpt-6-astra` 请求已实际出现该形态，协议/schema/测试核对见[证据](validation/native-cli-r1-native-interactions-2026-09-19.md#5-r2-必须覆盖的实际工具定义形态)。未知结构只保留脱敏且有界的请求说明，不默认解释成可执行命令。

#### 4.3.1 已实现的请求与模型身份

2026-09-19 [阶段证据](validation/native-cli-r1-chat-metadata-2026-09-19.md)验证以下封闭字段；2026-09-20 消息中同时保留已确认的模型来源，通过同一 itemKey 的 replace 更新。请求/响应 metadata 继续作为独立上下文事实。

```ts
interface RequestView {
  requestId: string;
  clientRequestIndex: number | null; // HTTP 为 null；WS 为本连接内 create 序号
  requestedModel: string | null;
  codexThreadId: string | null;
  codexTurnId: string | null;
  purpose: "conversation" | "auxiliary" | "unknown";
  purposeBasis: "codex_turn_metadata" | "missing_metadata"
    | "conflicting_metadata" | "unknown_metadata";
}
interface ResponseView {
  requestId: string;
  responseId: string | null;
  status: "receiving" | "completed" | "failed" | "incomplete";
  reportedModels: string[]; // 最多保留两个不同报告，冲突显式显示
  usage: ResponseUsage | null;
  usageConflict: boolean;
  observedDurationMs: number | null; // 本响应 created 至首个终态的观察时长
}
interface ResponseUsage {
  inputTokens: number | null;
  outputTokens: number | null;
  totalTokens: number | null;
  cachedInputTokens: number | null;
  cacheWriteTokens: number | null;
  reasoningTokens: number | null;
  invalid: boolean;
}
```

分类依据为 `client_metadata["x-codex-turn-metadata"]` 的 JSON 字符串：`request_kind=turn`、`thread_source=user` 且 thread/turn 有效才归为 conversation；prewarm/compaction/memory，或 turn 的 system/guardian_review/memory_consolidation 来源归 auxiliary。缺失、未知或与扁平 thread/turn/session 字段冲突则 unknown；冲突时清除原生身份并记录安全诊断。不能按 `role:user`、输出 schema、短文本或 JSON `title` 猜用途。

仅白名单字段进入 DTO：模型/标识最多 128 字符且经过格式与脱敏检查；thread 为非 nil UUID，turn 不接受路径分隔符。HTTP 请求在完整、有界 JSON 到齐后发布；压缩请求暂不能解码时显示缺失，原请求转发不变。WS masked frame 可以提取 create metadata，但 create 序号没有证明 response 归属，当前所有 WS 正文保留请求级阅读，不能按到达次序冒充聊天。

请求/响应 decoder 各自的活跃流上限默认 128，单 JSON/frame 默认 1MiB，共享 `push` 后保留缓冲上限 8MiB；这不是对单次输入解析期间峰值 RSS 的承诺。快照最多 256 条 request、256 条 response，淘汰会使阅读订阅重新快照。request metadata 只保存上述安全字段，不保留全量输入、header 或工作区；工具定义元信息与输出另走 §4.5.1 的有界 `toolContexts`。响应模型来自 response 报告，后续事件缺字段不清空已知值；报告冲突保留两个来源并标 partial。只有请求模型时明确标“请求模型”，两者不同同时显示。

### 4.4 用户提取、顺序与流式更新

1. **确认提交和作者。** 从已验证的请求输入结构或明确关联的 rollout 用户事件提取消息；Enter、PTY 回显、草稿和本地发送按钮都不是提交证据。系统/开发者指令、工具输出及框架注入的 `user` 上下文不生成用户气泡。请求结构无法区分人工输入时，R1 先接最小后台用户事件读取，不能以“以后 R2 再做”通过基础聊天验收；仍无证据时显示未知并记录能力限制。
2. **稳定身份与去重。** 网络正文沿用 request/response/wire Item/content index；用户事件优先用原生事件身份，否则仅采用 §4.1 已证实上下文链中的位置。相同历史在后续请求重复携带不新建消息，相同文字被用户再次提交仍是两条。重试 attempt、compact/fork、续传缺口或跨来源身份无法证明时不按文本/时间合并。缺少身份的内容留在请求级未归属区。
3. **保留交错顺序。** 已关联上下文和输出序号确定“用户 → 模型说明 → 工具 → 模型后续正文”；并发请求无法证明全序时按首次观察顺序分请求展示，标注“按观察顺序”，不猜 Turn。晚到用户/工具证据只有明确关联才插入对应位置。当前用户按已确认 turn 归组，模型/工具按首次观察 orderIndex 穿插；同轮多条用户提交标注与模型片段的精确先后未确认。内容补齐增加 revision、保持 itemKey 与阅读锚点，不把整段对话重新追加。时间戳只作说明。
4. **原位更新。** 首条带类型的 `item.replace` 创建卡片；`item.patch` 指明 `itemKey`、正文的 `contentKey` 或工具参数字段、baseRevision/revision 和 append，payload 使用封闭 union。工具结果、状态、模型名、来源补齐和最终全文均用 `item.replace` 原位替换。不同正文片段不能无序拼接；只含增补字段的来源事件先在 reducer 合并，不能擦除已捕获内容。
5. **重连与阅读。** 快照包含完整判别字段、稳定键、顺序及 revision；重复/乱序/缺口按 §5.2 处理。补齐来源不创建第二份气泡，展开状态按 `itemKey` 保留。上滚、展开结果或切面板不被新 token 抢走焦点；在末尾才自动跟随。迟到结果不假装其原执行时刻就是到达时刻。

#### 4.4.1 R1 最小用户来源接入

安装版 CLI 0.154.0 的隔离实验已证实：规范用户记录是 rollout JSONL 的 `event_msg` → `item_completed` → `item.type=UserMessage`，含显式 `thread_id`、`turn_id`、`item.id` 和 `content`；同一句话提交两次有两个不同 turn/item，分别对应模型请求 metadata。该版本并未在实验中输出旧 `event_msg.user_message`。`response_item` 中的 `role:user` 可含框架注入上下文，不能作为用户气泡来源。

最小 reader 已按以下边界接入 R1 调试运行，C01–C03 的基本 HTTP 对话与本机 Chrome 证据见[聊天验收](validation/native-cli-r1-user-chat-2026-09-19.md)：

- 在独立后台任务中读取 launcher 明确指定的 Codex home，只读操作；Web handler 和网络/PTY 转发不访问历史文件，也不等待 reader。
- 仅为本 Run 已观察、用途为 conversation 的明确 thread/turn 查找候选；文件名可作检索提示，但必须核验 `session_meta.id` 和事件自身 thread/turn。不能按 cwd/mtime 或“最近文件”绑定，不向页面输出真实路径。
- launcher 显式提供 home，启动时解析 canonical 根并固定目录描述符；后续子目录和文件均通过相对描述符 `openat + O_NOFOLLOW` 打开，拒绝非普通文件。200ms 一轮，遍历游标每轮最多 2048 个目录项、`sessions` 下最多 4 层目录、最多 8 个来源文件；所有文件合计每轮读取 256KiB。单行上限 1MiB，半行保留有界尾部；已验证 header 后的坏行隔离、超长行丢弃并报告缺口，后续有效行继续。
- 用户项稳定键使用原生 thread/turn/item ID；来源保留 opaque source ID、原文件位置及 ordinal。相同文本不去重，重读相同记录不重复；同键不同内容保留原证据并报冲突。通过 inode/设备、unlink、长度和读取尾部 guard 检查替换/截断/改写，检测到变化即停止该来源，已有消息可读，不自动续接新文件。这不是对任意历史字节被修改的完整内容认证。
- 首版只接已确认的文本，保留中文、空行和代码字符；非文本/未知内容以省略提示保留不完整性。进入 LiveStore 前脱敏，页面按转义文本呈现。单条预览最多 64KiB；未来 turn 的安全待关联记录最多 128 条/1MiB，LiveHub 用户区最多 256 条/4MiB，来源诊断最多 32 条。超限明确标缺口，淘汰网页用户项后订阅必须重新快照。
- 用显式 thread/turn 将迟到用户证据放在对应回复前；不能通过正文、时间或位置推断归属。补齐不重挂终端、不抢焦点、不改变上滚阅读锚点；相同项的替换保留稳定 DOM key。

以下 `UserRecord` 是原生 reader 与 LiveHub 之间的内部来源契约。对外已映射为 §4.3 的 user MessageItem，不再输出 userMessages 数组或 user.replace 事件；来源位置与省略标记完整保留，展示仍仅按明确 HTTP 轮次关系归组。

```ts
interface UserRecord {
  key: { codexThreadId: string; codexTurnId: string; nativeItemId: string };
  role: "user";
  text: string;
  revision: number;
  truncated: boolean;
  omitted: boolean;
  source: { sourceRef: string; byteOffset: number; ordinal: number | null };
}
interface UserCapture {
  enabled: boolean;
  diagnostics: { sourceRef: string; byteOffset: number; code:
    "invalid_line" | "line_too_large" | "missing_identity" | "identity_conflict"
    | "source_changed" | "read_failed" | "capacity" | "unsupported_tool_evidence" }[];
}
```

只有本 Run 已观察的 conversation thread/turn 才发布气泡，不展示该文件中的旧 turn 历史。未来用户事件早于 metadata 时仅留在有界待关联区。按轮次插入迟到气泡、稳定 DOM key 和可见卡片锚点，已通过延迟 reader 4 秒的浏览器检查。每条模型回复都可显示“尚未取得本轮原生用户提交记录”。同一 turn 多条用户事件能分别阅读，但与模型片段的跨来源精确先后未确认，界面明确标“按来源归组，先后尚未确认”；不得把分组冒充完整全序。

#### 4.4.2 已验证的复杂上下文边界

2026-09-20 的[安装版 CLI/Chrome 证据](validation/native-cli-r2-contexts-2026-09-20.md#native-context)补齐 compact、fork 和显式 resume：摘要保留在后续模型 input，按 compaction 用途留在请求上下文；同文四次原生提交分别有新的 item 身份。fork 改变原生 thread，显式 resume 保留 fork 后 thread、更新 Run epoch；恢复本身不生成新请求或用户气泡。新 Run 中的新提交才进入当前阅读。

同轮 steering 的两次模型请求共享明确 thread/turn，原生 UserMessage item 不同；终端草稿不产生气泡，提交后分别显示。该验证保留上述顺序未确认说明。合成 WS create 与响应并发时只按明确 response ID 更新内容，previous_response_id 作为请求参数可读，不据此为当前 create 猜新响应或跨请求挂接工具结果。详见[并发与用量验证](validation/native-cli-r2-contexts-2026-09-20.md#concurrency)。

合成测试覆盖半行/坏行/超长行、同文不同提交、注入上下文排除、冲突身份、重读/截断/替换、符号链接/FIFO 拒绝、脱敏、缓存上限及 SSE 快照游标；安装版 CLI＋本机 Chrome 覆盖真实角色数量、基本双轮顺序、草稿排除、刷新不重复及焦点/锚点。当前仅适配受测 CLI 的规范 `UserMessage`、`source=cli/thread_source=user`，后台只读 `sessions` 与 `archived_sessions`。R3 支持按原文件身份识别归档移动，继续使用原偏移且不重复用户记录；退出时残留半行标 `partial_line`。reader 的退出等待上限为 1 秒，超时让线程分离，不阻塞 CLI 退出；系统调用本身不可强制取消。旧 schema、无法验证的文件替换和任意旧 turn 的全库导入不属于当前能力。

### 4.5 工具、命令与结果状态

调用参数状态和执行状态独立呈现，避免一个绿色勾同时表示“生成结束”和“执行成功”。工具结果可以晚于模型响应结束，也可能始终无法观察；下表给出允许的状态依据。

| 已观察事实 | 执行状态 / 文案 | 不得推断 |
| --- | --- | --- |
| 工具参数 delta、完整 function/custom tool call | `unobserved`：尚未观察到执行；参数另标生成中/已生成 | 已开始执行、等待审批或成功 |
| 明确关联的原生执行开始事件 | `running`：执行中 | 用参数生成完成或无 token 的等待时间代替开始证据 |
| 匹配 call 的 output，但没有可靠成败字段 | `result_observed`：结果已观察，执行状态未确认 | 任意 output 都代表成功；正文写了“成功”就可信 |
| 结构化结果或已验证原生结果格式确认退出 0 / 工具成功 | `succeeded`：执行成功；命令显示退出码 | 命令成功代表整个任务完成或所有拟议修改均已落盘 |
| 明确非零退出码或工具失败事实 | `failed`：执行失败；展开错误和已有输出 | 文本含 error 就一定失败，或无输出就一定成功 |
| 明确关联的原生 `CommandExecution.status=declined` | `declined`：已拒绝执行 | 把拒绝当作执行成功，或用等待时间猜测审批状态 |
| 与调用匹配的原生取消事实 | `cancelled`：已取消 | 模型 response 失败/取消会自动取消每一个工具 |
| 输出截断、观察丢失、未知归属 | 保留最后有证据的状态并显示 partial/截断/未归属 | 用缺失证据补出成功或完整输出 |

工具身份至少由 Run、已确认的上下文/调用域和 call ID 约束，不跨请求链仅凭同名 `call_id` 合并。后续请求的 `function_call_output` / `custom_tool_call_output` 只有在调用域可证明时更新原卡；无匹配、call ID 冲突或结果先到时保留未归属结果，证据补全后用同一 `itemKey` 替换占位或合并到明确调用。证据冲突保留双方来源并提示，不按“最后到达者赢”覆盖。

命令卡的标题、命令预览、参数与结果分区独立；cwd 未提供就不猜，stdout/stderr 未分流就显示“合并输出”，退出码与耗时未捕获就显示“未捕获”。非零结果默认露出失败摘要，长输出折叠、限制高度并注明截断；仅当完整内容实际可读取时提供详情入口。R1/R2 没有落盘副本时不能声称“完整日志已保存”。

用户文本保留换行；模型正文支持安全 Markdown；参数、命令和输出按转义文本/代码呈现，不运行 HTML/SVG、ANSI 或脚本。所有内容先脱敏。角色与工具类型用可读文字和不同图标/边框区分，键盘可展开详情，屏幕阅读器只播报节制的状态变化，不逐 token 打断。可见 reasoning summary 若接入须独立折叠并标来源，不混为最终回复。

#### 4.5.1 已实现的工具流与结果增量契约

2026-09-19 [工具阶段证据](validation/native-cli-r2-tools-2026-09-19.md)覆盖安装版 CLI 经正式 binary 的 Code Mode 与直接命令路径，[原生结果增量](validation/native-cli-r2-native-results-2026-09-19.md)补充迟到退出与长输出。2026-09-20 起快照中工具属于 `items: ViewItem[]`，独立 `toolContexts: ToolContext[]`、`nativeCommands: NativeCommand[]` 继续保留；具体字段见[前端封闭类型](../web/src/workbench/toolTypes.ts)、[后端工具视图](../src/workbench/live/tools.rs)和[上下文提取](../src/workbench/decode/tool_context.rs)。统一 union 保留以下实际工具字段，不引入 file/search/cancelled 等没有当前事实来源的类别或状态。

```ts
interface ToolCall {
  key: TextKey;               // requestId / responseId / wireItemId / contentIndex
  kind: "tool_call";
  toolKind: "function" | "custom";
  callId: string | null;
  name: string | null;
  namespace: string | null;
  category: "command" | "code" | "patch" | "other";
  arguments: string;          // 脱敏、有界；最终全文原位替换
  argumentsState: "receiving" | "generated" | "incomplete";
  command: { text: string; cwd: string | null } | null;
  proposedPatch: ProposedPatch | null; // §4.5.2
  execution: "unobserved" | "running" | "result_observed" | "succeeded" | "failed" | "declined";
  result: {
    source:
      | { kind: "model_request"; requestId: string; clientRequestIndex: number | null; inputIndex: number }
      | { kind: "native_rollout"; sourceRef: string; byteOffset: number; nativeItemId: string; processId: string | null };
    output: string; exitCode: number | null; durationMs: number | null;
    streams?: { stdout: string | null; stderr: string | null }; // FileChange 的独立流；此时 output 为空
    truncated: boolean; omitted: boolean;
  } | null;
  revision: number; orderIndex: number; captureSeq: number;
  truncated: boolean; identityConflict: boolean; resultConflict: boolean;
}
```

- **生成与顺序。** function arguments/custom input 的 added、delta、done 和完整 output item 进入独立 decoder；完整参数替换预览，流中断标 incomplete，单次 response completed 不证明执行。正文与工具共享首次观察 `orderIndex`，结果只替换原卡；该顺序不承诺并发请求的原生全序。SSE 与带显式 response 身份的 WS 工具流可独立解码；WS create 归属未证明，仍留在请求级阅读且不跨请求关联结果。
- **类别与参数。** 顶层 `tools`、developer `additional_tools` 与 namespace 均有安全元信息提取。原生 `exec_command` 的 function/object schema/string cmd 才映射命令；原生 grammar `exec` 映射代码、`apply_patch` 映射拟议修改，其余保留普通工具。缺 namespace 时仅允许无歧义定义匹配，外部 namespace 的同名工具不冒充原生命令。仅完整、未截断且无身份冲突的安全 JSON 提取 cmd/workdir，cwd 未给就显示未捕获。
- **结果关联。** 只接受已确认用途、同一原生 thread 的 HTTP 请求域。后续请求必须含唯一的同 call ID、同工具类型/名称/namespace 的伴随调用，并与已观察调用的原始完整参数指纹一致；本 Run 中候选也必须唯一。指纹、原始参数和内部解析事实不随 DTO 输出。缺伴随调用、WS 或身份不明时，结果保留在对应请求上下文。先到结果可在调用补齐后关联；后续重复历史不新增卡片或重复 revision。
- **执行依据。** 后续请求中的直接命令结果只识别已验证且从输出首部开始的原生 `Wall time` / `Process exited with code` 或 `Process running with session ID` / `Output:` 格式；正文中出现相同字样不作为退出事实。Code Mode 返回的 JSON/文本不递归解析成子命令状态，只标 result_observed。运行中 envelope 保留进程 ID 供原生结果匹配；`write_stdin` 卡保留自身 call ID，原命令通过规范完成记录取得最终结果，不拼接两个不同调用的文本。
- **原生补齐。** 按 [ADR 0041](decisions/0041-native-command-result-evidence.md)，独立 RolloutReader 读取规范 `event_msg/item_completed/CommandExecution`。只接受明确 thread/turn/native item ID 和已知 source/status；同一 HTTP conversation 的 thread + turn + call ID 唯一匹配，已有进程 ID 时还须一致。已验证安装版命令在最后一次模型请求之后退出，原卡能取得最终合并输出、退出码、耗时和 cwd。未匹配子命令在同轮调用详情独立保留，不按 Code Mode 代码或 ID 前缀猜归属。`completed/failed/declined` 有封闭映射，受测原生流程未取得 declined 或可靠 started；规范支持不等于实际捕获。[原生策略拒绝、参数错误与中断](validation/native-cli-r2-contexts-2026-09-20.md#native-lifecycle)实测没有相应逐工具终态，UI 保持 result_observed/unobserved，turn_aborted/EOF 不生成工具取消。底层 declined 也可能来自运行准备拒绝，不默认标成“用户拒绝”。
- **原生去重与冲突。** 原生结果同键同内容指纹不重复发布；相异最终记录保留各自 sourceRef/byteOffset，标冲突并撤销明确成败。网络旧运行中证据、重放或迟到 started 不降级已有最终状态；网络最终退出码与原生退出码矛盾、重复候选或进程不一致均不能静默覆盖。
- **稳定阅读身份。** 同一 response 中明确同时出现 output_index 与正式 item ID 时建立有界别名，保留首次 key；`TextKey.wireItemId` 可以继续为 `@output:N`，不为了补协议 ID 更换 DOM 身份。之后仅带 ID/位置的事件原位更新，最终替换不追加。别名表共用 decoder 的 256 项预算且计入缓冲总量；SSE、并发 WS、正文与工具及实际 Chrome 节点/焦点/刷新检查见[身份增量证据](validation/native-cli-r2-content-identity-2026-09-19.md)。没有显式关联时不凭最近项或 call 名称猜配。
- **冲突。** 同键身份变化、同调用不同参数、重复 call ID 候选或相异结果明确标冲突并撤销成功状态，已有参数/结果保留，其他输出仍可在调用详情的上下文查看。位置/正式 ID 一对多、类别变化或两张已独立发布卡片事后出现矛盾桥接，保留原内容并发身份诊断，不静默合并。显式无效/敏感 ID 不退回位置编号；HTTP 显式 response ID 与已确认 ID 不同也拒绝更新。最终工具身份缺失或被脱敏拒绝时，不允许用旧参数指纹为新内容背书。
- **预算与安全。** 工具参数、单结果预览各限 64KiB；工具与模型正文共享 512 项/32MiB 缓存。每请求扫描最多 4096 个 input，工具定义最多 256、结果最多 128；工具上下文最多 64 请求项/4MiB。原生命令待关联区 128 条/1MiB，LiveHub 原生记录 128 条/4MiB；原生 argv 另限 64KiB/128 项，cwd 4096 字节，源行最大 1MiB。淘汰标 partial 并要求重新快照。先脱敏再裁剪：已知凭证跨分片隐藏，常见敏感字段支持无引号、空白、camelCase 与 ASCII Unicode/hex 转义及嵌套引号，识别后保守省略后文，非文本结果以 omitted 提示。不是代码解释器，不能识别任意计算生成或未知编码的秘密。
- **截断。** 原生 `... N bytes omitted ...` 整行、`…N tokens truncated…` / `…N chars truncated…` 和受测 warning 首行进入 truncated 提示；普通 token count 不算截断。这些文本也能由工具主动打印，因此文案为“输出预览超限或来源含截断标记”，只降低完整性声明，不推断执行成败。超过 64KiB 的原生结果已通过 CLI/Chrome 有界显示检查，C10 的 R2 受测范围与后续 provider/设备边界见[整片验收](validation/native-cli-r2-acceptance-2026-09-20.md)。

`NativeCommand` 使用 UserKey/UserSource 的明确来源字段，另有 `status/processId/commandSource/command/cwd/output/exitCode/durationMs/truncated/omitted`；原始指纹不序列化。原生事件顶层 requestId 为空、captureSeq 为 0，不把源文件事件伪装成网络观察序号。文件变化/非法记录等诊断沿用 `userCapture` 通道并新增 `unsupported_tool_evidence`，页面按“原生来源”显示来源位置，后台覆盖用户提交、命令与文件修改记录。

UI 使用不同布局、图标和文字标签呈现用户、模型及工具，代码块不会制造命令卡。调用参数与结果分别展开；失败/拒绝结果默认展开，未分流输出明确说明 stdout/stderr 未区分。可键盘展开，普通更新/面板切换保持已展开详情，刷新恢复同一 Run 的卡片与状态；R3 持久化已有安全预览；来源或预览本来就截断时，仍明确完整参数/日志未保存在工作台。schema、系统/消息上下文与响应用量已按 §5.1.1 接入按需阅读，结构化拟议 Diff 见 §4.5.2。受测工具事实和复杂对话关联见 [R2 验收](validation/native-cli-r2-acceptance-2026-09-20.md)，没有可靠证据的终态继续保持未知。

#### 4.5.2 结构化拟议 Diff（已实现）

[纯读取 parser](../src/workbench/decode/patch.rs)在已有原生工具定义证明 `custom apply_patch` 后，解释完整、未截断且无身份冲突的安全参数。它不解析 Code Mode 内嵌代码、不读写文件、不查 Git，也不推断工具成功。数据随原 `item.replace`/snapshot 发布，不新增网络读取；只有类别、参数、生成状态、截断或身份发生变化才重新计算。定义晚到可补齐，定义变得不明确则撤销结构化预览。

```ts
type ProposedPatch =
  | { state: "ready"; environmentId: string | null; files: ProposedFile[] }
  | { state: "unavailable"; reason: "incomplete" | "truncated" | "identity_conflict" | "unsupported_format" | "budget" };
interface ProposedFile {
  operation: "add" | "update" | "delete";
  path: string; moveTo: string | null;
  addedLines: number; removedLines: number | null;
  sections: {
    anchor: string | null; atEof: boolean;
    lines: { kind: "added" | "removed" | "context"; text: string }[];
  }[];
}
```

生成中的参数继续按文本阅读，`proposedPatch=null`。`ready` 仅代表完整预览：保留显式增删行，即使两行文字相同也不合并；`@@` 仅是上下文锚点，不伪造文件行号。支持新增空文件、修改、移动并修改、整文件删除、EOF、环境 ID、CRLF 与常见 literal heredoc。整文件删除不带旧内容，`removedLines=null`，页面显示“删除行数未知”。超限、未知格式或脱敏省略导致无法解析时不展示局部 Diff，保留捕获参数入口。

上限为参数 64KiB、输入 4096 行、64 文件、256 片段、2000 个增删/上下文行；路径 4096 字节、环境 ID 128 字节。结构化内容的内存估算计入工具与正文共享预算，淘汰继续使用原有缺口/重新快照机制。路径、锚点、内容均作为字面文本，不提供执行、打开文件或跳转链接；文件阅读属于 R4。

[Diff 组件](../web/src/workbench/ProposedDiff.tsx)使用操作名称、增删符号、计数和颜色共同区分，首文件默认展开，其余可键盘展开；长内容在有界区域滚动。参数完成、迟到结果和面板切换不重建原卡及已展开节点，不抢焦点。[安装版 CLI / Chrome 验收](validation/native-cli-r2-proposed-diff-2026-09-19.md)核对了临时文件四种操作及缺失文件失败。规范 FileChange 状态已按 §4.5.3 补齐；只有 output 正文时继续标 `result_observed`，不得把预览或输出中的“Success”当作已验证的执行状态转换。

#### 4.5.3 原生文件修改结果（已实现）

按 [ADR 0042](decisions/0042-native-file-change-evidence.md)，同一只读后台 reader 提取规范 `event_msg/item_completed/FileChange`，仅接受明确 thread/turn/item ID 及 closed status。缺失/未知身份或状态发来源位置诊断。与模型调用的关联要求明确 HTTP conversation 域、唯一 call ID、已生成且类别已由工具声明确认的原生 custom apply_patch；Code Mode 和外部同名工具不猜归属。

```ts
interface NativeFileChange {
  key: UserKey;
  source: UserSource;
  status: "completed" | "failed" | "declined";
  files: { path: string; operation: "add" | "update" | "delete" | "unknown"; moveTo: string | null }[];
  stdout: string | null; stderr: string | null;
  truncated: boolean; omitted: boolean;
}
```

当前安装版 CLI 已验证成功与执行阶段失败的规范记录；前置校验错误只有后续 output，仍为 `result_observed`。修改审批的 Esc 已验证为 turn_aborted 且没有工具完成记录，不能推断 declined/cancelled；规范 declined 有 fixture 映射，实际来源继续受支持矩阵限制。证据见[文件结果验收](validation/native-cli-r2-file-results-2026-09-19.md)。

同键同内容指纹去重；相异终态保留各自 sourceRef/byteOffset，原卡变为冲突/执行状态未确认。定义变化使原生工具类别失去证明时同样撤销确定状态，保留结果。结果早到或迟到不改变卡片 key、拟议 Diff 和展开状态；已有原生结果不会被后续请求中的旧输出替换。未匹配记录在同轮调用详情单独可读。

`ToolResultView.streams` 仅在原生记录明确分流时提供，此时通用 `output` 为空，退出码/耗时保持 null。stdout/stderr 各 32KiB，空串显示空输出、null 显示未捕获；错误输出不会因 stdout 超长而失去全部预算。原生路径按字面呈现，最多 64 文件、每路径 4096 字节、路径总量 64KiB；完整文件内容和 native unified_diff 不进入此 DTO，不声称 paths 是实际已落盘的完整文件列表。未知文件变体标 unknown/omitted。

待关联文件事件最多 128 条/1MiB，LiveHub 最多 128 条/4MiB，源行仍限 1MiB；工具卡内的分流结果也计入共享阅读预算。超限和文件变化沿用原生来源诊断、partial 与重新快照机制，不阻挡转发。此增量不读取 Git 或实现文件入口。

## 5. 直接推送、快照与重连

### 5.1 新 API 与实时契约

```text
GET  /workbench/v1/run
GET  /workbench/v1/live/snapshot
GET  /workbench/v1/live/events?epoch=<runEpoch>&after=<viewSeq>
GET  /workbench/v1/requests/{requestId}?epoch=<runEpoch>&cursor=<opaque>
GET  /workbench/v1/history?cursor=<opaque>
GET  /workbench/v1/history/{runEpoch}?before=<viewSeq>
GET  /workbench/v1/history/{runEpoch}/requests/{requestId}?cursor=<opaque>&before=<viewSeq>
WS   /workbench/v1/terminal?epoch=<runEpoch>
POST /workbench/v1/run/stop
```

控制接口只服务当前 Run；历史接口读取当前项目的已保存 Run。初版没有网页创建任意 CLI、通用 shell 或 JSON-RPC 透传路由。当前 mutation 包括输入仲裁、本次运行停止、工作台 JSON 配置保存及历史清理任务；原生发送、approval、queue 和模型/权限 settings 全留 CLI。Mutation 校验配对身份、Origin/CSRF 和本次 runEpoch，旧页面不能误停端口复用后的新 Run；stop 对同一 Run 可重复且不创建新进程。新 namespace 与旧 `/v1`、`/v2` 明确区分。R3.1 已接入工作台配置保存、占用查询/刷新、候选预览、删除任务及可选保留期限，完整契约见[管理 API](codex-native-cli-workbench-history-settings.md#62-管理接口契约)。

终端接入时将原草案的 claim/release/takeover 三个 POST 收敛为上述 WebSocket 内的类型化消息，原因是操作必须绑定实际终端连接，私有 grant 也只应回给该连接；不增加用于跨 HTTP 请求认领连接的第二套 token。尚未发布的 POST 草案没有兼容消费者，旧 `/v1`、`/v2` 不变。运行停止仍为 HTTP POST，JSON body 为 `{ "epoch": "<runEpoch>" }`。

```ts
interface LiveSnapshot {
  runEpoch: string;
  viewSeq: number;             // 与快照内容在 LiveStore 同一临界区取得
  schemaVersion: 2;
  items: ViewItem[];           // 有界、安全 DTO
  capture: "ok" | "partial" | "unavailable";
  recorder: "disabled" | "pending" | "saved" | "degraded";
  recorderStatus: RecorderStatus;
  persistedThroughViewSeq: number;
  historyCoverage: "partial" | "complete_for_observed_scope";
}
interface RecorderStatus {
  runEpoch: string;
  state: "disabled" | "pending" | "saved" | "degraded";
  observedViewSeq: number;
  persistedThroughViewSeq: number; // 已同步的连续前缀，遇到缺口停止推进
  savedThroughViewSeq: number;     // 最近独立分段已同步位置
  historyCoverage: "partial" | "complete_for_observed_scope";
  gapCount: number;
  error: string | null;            // 固定安全诊断，不含正文或本机路径
}
interface ViewEnvelope {
  runEpoch: string;
  viewSeq: number;
  requestId: string | null;
  captureSeq: number;
  // 与下文 ItemChange 或来源事件的 kind/内容字段同层，没有通用 payload 字段
}
```

实时流使用单条 SSE，正文、工具参数、状态都带实际 payload。小更新可合并 16–33ms 一批；append 合并只限同身份相邻片段，不能 latest-wins 丢掉独立 append。首次内容、结束和错误不等待额外合并。大请求上下文与历史才按需 GET，不能恢复“每个通知再查数据库”的方案。

Recorder 健康与保存水位作为同一连接的独立 `recorder.status` 控制消息发送，不分配内容 viewSeq、不再次写 journal；否则“保存成功”通知会不断制造新的未保存内容。重连时以快照和当前 recorder 状态为准。

正式 Launcher 已接入 R3 Recorder。底栏优先显示 §5.1.2 的用户用量与简短保存状态，保存异常/历史缺失不被折叠；水位与技术故障说明位于“诊断详情”。`saved` 只表示已观察副本已确认保存，不代表捕获完整或整个任务结束。未安装记录器的旧 R0/R1 合成演示仍为 `disabled`。恢复后连续水位与新分段水位分开，底栏显示“历史有缺失”，诊断中保留原“已保存 · 曾有缺口”。上下文文档也参与待保存判断，即使 viewSeq 没有增加也不能提前显示已保存。

当前快照的 `schemaVersion=2`，`items: ViewItem[]` 是唯一消息/工具/notice 列表；另含 requests、responses、diagnostics、userCapture、toolContexts、nativeCommands、nativeFileChanges 这些来源和状态数据。旧 tools/userMessages 数组已移除。`/workbench/v1/run` 的 scope 为 typed-conversation-items，并返回 readingSchemaVersion=2 及 `historyAvailable`。未发布的 R1/R2 阅读契约在本次同步升级，旧 `/v1` 不受影响；旧页面应刷新加载新脚本。

SSE 的 view envelope 保留 runEpoch/viewSeq/requestId/captureSeq。项目内容只使用以下封闭变体：

```ts
type ItemChange =
  | { kind: "item.replace"; item: ViewItem }
  | { kind: "item.patch"; itemKey: string; baseRevision: number; revision: number;
      field: "text"; contentKey: string; append: string; truncated: boolean }
  | { kind: "item.patch"; itemKey: string; baseRevision: number; revision: number;
      field: "arguments"; append: string; truncated: boolean };
```

首个项目只能由 replace 创建。patch 要求项目已存在、类型与 field 匹配、正文 contentKey 存在、baseRevision 等于当前版本且 revision=baseRevision+1；失败重新快照，绝不凭 patch 推断角色。旧 replace revision 不回退；最终全文、模型来源/状态、工具结果和冲突均由同键 replace 更新。snapshot 与 live 的表示完全相同。

| 其他 kind | 内容字段 | 应用规则 |
| --- | --- | --- |
| `request.metadata` | request | HTTP 请求或 WS create 的安全 metadata，不从消息正文猜用途 |
| `request.state` | response | 独立响应状态、模型报告、usage/冲突与 observedDurationMs；受影响消息另发 replace |
| `tool.context` | context | 按 requestId/clientRequestIndex 保存有界定义与工具 output 来源 |
| `native.command` | command | 按 sourceRef/byteOffset 保留原生证据，明确匹配后另用 replace 更新原工具 |
| `native.file_change` | change | 同样保留原生文件修改证据，不从路径/正文猜状态 |
| `user.capture` | userCapture | 原生后台读取的启用/缺口状态 |
| `capture.gap` | diagnostic | 标记捕获不完整；同一来源另有稳定 notice 项 |

rollout 原生事件顶层 requestId=null，不伪造网络 ID。所有项目与来源事件参与同一 viewSeq/ring 接续；来源淘汰使已有订阅重新快照。后端项目预算沿用正文/工具共享 512 项/32MiB、用户 256 项/4MiB、诊断 128 项，正文单项 1MiB；统一 DTO 不复制一份长期存储。请求详情仍按需读取独立缓存，history endpoint 和 Recorder 已按 §7 接入。

#### 5.1.1 R2 按需上下文与响应用量（已实现）

`GET /workbench/v1/requests/{requestId}?epoch=<runEpoch>&cursor=<opaque>` 读取当前 Run 的脱敏内存副本。沿用配对 Cookie、严格 Host/Origin/Fetch-Site 边界和 `Cache-Control: no-store`；epoch 必填，未知/重复 query 字段和无效 UUID 拒绝，cursor 最长 160 字节。此 API 不读取磁盘、不访问模型、不取得终端输入权。

```ts
type DetailSource =
  | { kind: "request"; clientRequestIndex: number | null }
  | { kind: "response"; responseId: string | null };
interface RequestDetailsPage {
  runEpoch: string;
  requestId: string;             // HTTP 事务或 WS 连接的观察 ID
  revision: number;              // 上下文缓存版本，不是 viewSeq / 持久水位
  availability: "captured" | "pending" | "unavailable";
  requestCaptured: boolean;
  responseCaptured: boolean;
  truncated: boolean;
  omitted: boolean;
  conflict: boolean;
  captureIssues: string[];       // 本请求最多 16 个安全诊断码
  totalEntries: number;          // 当前保留项数，不冒充源文档完整项数
  entries: Array<{
    source: DetailSource;
    captureSeq: number;
    section: "settings" | "instructions" | "input" | "tools" | "response" | "output";
    position: number | null;
    jsonPointer: string;         // 原请求/响应位置；空串表示根级字段投影
    childrenSeparated: boolean; // additional_tools / namespace 的定义单独列出
    preview: string;             // 脱敏 JSON 文本；截断时不保证可重新 parse
    truncated: boolean;
    omitted: boolean;
  }>;
  nextCursor: string | null;
}
```

请求按白名单读取模型设置、instructions、input 和 tools；原始认证头、client metadata 和未知顶层扩展没有详情输出路径。响应读取 id/model/status/usage/service_tier/error/incomplete_details 与 output，缺失 output 明确标省略。HTTP 请求与各个 WS create、各 response 文档独立保留；create 序号不证明 response 归属，缺 ID 不借用上一次 WS 响应。相同来源及相同安全内容去重，相同来源不同内容同时保留并标冲突。请求中的 system/developer/user 历史不创建聊天气泡。

开发者 `additional_tools` 与 namespace 按工具定义拆成独立项，`jsonPointer` 保留 `/input/N/tools/...` 或 `/tools/...` 原位置。集合父项不再重复承载全部子工具，`childrenSeparated=true` 提示向后查看。大集合回归证明，整组放入单项预览会把后面的 schema 截掉，因此按定义分页。安装版 Code Mode 的 exec 是 custom tool，保留其原始 format；内部 exec_command 的文本说明不转换成不存在的结构化 schema。直接命令模式则读取实际 function parameters。

每项先处理已知凭证和常见敏感字段，再限制预览为 24KiB；单文档估算 512KiB、最多 1024 项，树遍历最多 8192 节点/12 层、数组/对象各 256 项。媒体/文件数据、加密内容省略；schema 保留字段声明，default/examples/未知扩展不展示，敏感参数声明只保留类型、说明等元信息。JSON/HTML/ANSI 均作为不可信内容，网页使用转义的 `<pre>`，不执行其中脚本。不能承诺识别任意代码、未知编码或未配置的秘密。

缓存最多 64 个请求/连接 bundle，总估算 16MiB，每 bundle 2MiB/128 份文档；淘汰保留最多 256 个 tombstone，再出现时说明截断。每页最多 16 项/128KiB，与 snapshot 共用 2 个读取配额，响应 body 消费或丢弃后才释放。没有文档但仍接收的已知响应返回 pending；存在诊断或已无法取得的已知文档返回 unavailable；有部分文档时返回 captured，并分别说明 request/response、省略、截断与诊断。unknown 为 404、已知淘汰为 410、非法 cursor 为 400、epoch 或文档版本变化为 409、读取繁忙为 503。cursor 绑定 epoch/request/revision/offset，不能跨来源或拼接不同版本。

文档不进入 snapshot 或 live ring，不分配 viewSeq；先放入详情缓存，再发布 request metadata / response 终态，因此客户端看到终态后可读取对应文档。观察解析和缓存仍在独立后台路径，网络/PTY 不等待它们；R3 记录器另外保存安全文档并分配 recordSeq，仍不分配 viewSeq；详情缓存本身不冒充已保存历史。

页面通过模型回复/工具卡的“调用详情”直接读取，或从“用量概览 → 查看调用记录”选择请求。独立“模型请求/网络”页面与重复导航已移除，列表不复制对话正文/工具卡；仅辅助及未归属内容在详情折叠保留。中央侧面板同一时间保留一个请求的详情，原终端持续可用；单次模型调用只有工具卡时同样可进入。普通 token 更新不触发 GET，新响应状态提示手动刷新；失效分页保留旧内容并提示刷新，不混入新版本。前端每组最多 256 项/2MiB，达到上限可替换当前组继续阅读后续页。关闭详情/换请求释放内容并取消在途请求；旧 epoch/request 的返回不得覆盖当前选择。

usage 仅来自可归属 response 的终态报告，各响应独立，重复事件不累加、缺字段不清空已知值、冲突保留首份并提示核对。缺失为 null，零保留为零；负数、非整数、超 JavaScript 安全整数或明显自相矛盾的计数标 invalid。缓存命中/推理是明细，不重复加到总量；没有价格估算或任务总量。`observedDurationMs` 使用同响应 created 到首个终态的 monotonic 观察时间，缺 created/身份则未知，不能冒充模型计算、TTFT、工具执行或整个任务耗时。官方源码依据为研究基线 `633ab199...` 的 `codex-api/src/sse/responses.rs` usage 解码；保留缺失和冲突是本项目的阅读选择。

API、合成 Chrome 与安装版 CLI 的范围和结果见[请求详情增量验证](validation/native-cli-r2-request-details-2026-09-19.md)。复杂对话关系及整片 R2 仍按实施计划验收。

#### 5.1.2 本次运行用量概览

按 [ADR 0046](decisions/0046-user-facing-run-usage.md)，在单次响应事实之上新增“本次运行累计已知用量”。保留单次响应详情，取消“不可跨响应求和”的界面限制；合计不代表某个原生任务的完整用量，也不求和观察时长。`usageSummary` 是 snapshot 与 `request.state` / `capture.gap` 事件中的可选附加字段，不改变 ViewItem schemaVersion=2：

```ts
interface UsageMetric { tokens: number | null; responses: number }
interface UsageSummary {
  responseCount: number;
  missingResponses: number;
  excludedResponses: number;
  inputTokens: UsageMetric;
  outputTokens: UsageMetric;
  totalTokens: UsageMetric;
  cachedInputTokens: UsageMetric;
  cacheWriteTokens: UsageMetric;
  reasoningTokens: UsageMetric;
  captureIncomplete: boolean;
  unidentifiedResponse: boolean;
  capacityExceeded: boolean;
}
```

`responses` 是提供该项合法数据的响应数，不能用一个覆盖率掩盖各字段缺失。没有报告时 tokens=null；合法零保留 0。内部用宽整数累计，若合计超过 JavaScript 安全整数则 tokens=null、responses>0，页面显示超范围说明。总量优先用合法 total；缺 total 且 input/output 都存在时可求和。缓存命中、缓存写入和推理明细均不再追加到 total。

观察层独立保存最多 16,384 个 `(requestId, responseId)` 的数值去重资料，不随 256 项响应预览淘汰。每次报告更新增减对应贡献，重复不计数；缺报告不清空已知值；非法或冲突响应从所有合计排除并增加 excludedResponses。未知身份的终态不建立可计数响应，标 unidentifiedResponse。不同请求尝试仍是不同观察响应，不把该数值冒充上游账单；HTTP/WS 都必须有明确 response 身份。达到容量后忽略新身份并标 partial，保留旧身份的去重和修正能力。

浏览器收到摘要整份替换，刷新直接使用快照，不扫描当前消息或对历史重算。Recorder 沿用异步 snapshot/journal 保存同一摘要；旧历史缺字段仍可读，不推导未知的历史累计，新运行另起统计。没有数据库 migration、新配置或额外轮询端点；当前 Run 的底栏不会切换成所选历史的用量。

底栏显示精简数字与保存状态；展开后显示精确数字、统计覆盖和异常说明。运行 ID、PID、连续/分段保存水位、解析记录放入默认折叠的诊断区。政策省略媒体不自动等同于 Token 缺失；网络观察缺口会标记统计范围可能不全。终端节点、输入权和停止确认保持原语义。

### 5.2 接续规则

LiveStore 单一 reducer 按序更新状态、分配 `viewSeq` 并放入 ring。快照捕获内容和水位；其后订阅先在同一调度边界注册，再重放 `(snapshotSeq, currentSeq]`，接续后续 live。ring 淘汰之前的 cursor、epoch 不匹配或 revision 不连续都返回 `snapshot_required`，浏览器重新快照。

Browser 只在完整应用事件后推进 cursor；重复事件按 epoch/seq 忽略。patch 基础 revision 不符须快照，不盲目追加。慢消费者耗尽自己的队列后断开，恢复不影响 CLI；客户端在后台节流或离线时不承诺实时延迟。snapshot/read API 不要求终端输入权。

内存快照可以比 journal 更新；刷新同一存活 Run 不等磁盘。整个服务重启只读取已保存资料及能确认的 rollout，旧 run 标 ended/unclean，生成新的 epoch；不能宣称活跃 PTY 可恢复。日志 cursor 不授予输入权。

## 6. PTY 与单写连接

单写权保存在内存：`controllerConnectionId + generation + reconnectSecret`。首个配对页面完成终端快照恢复后，对空闲终端自动 claim；收到 grant 才转发按键。日常界面不显示“启用输入”或“释放输入权”，输出始终可读。断线保留约 30 秒重连窗口，原页面凭私有 reconnectSecret 重新绑定并轮换 generation；窗口过期空出写权，等待页面可随状态更新自动 claim，每个 generation 只请求一次，不轮询争抢。

只有另一页面占用或处于重连保留期时，才显示“另一页面正在使用此终端。切换后，另一页面将暂停输入。”及“在此输入”。用户点击一次发起 takeover，不再叠加确认弹窗；收到 grant 后聚焦终端，原页面立即只读。不得自动 takeover。以上时间是体验参数，不是持久 InputLease/TurnOwner。

输入帧含 generation 和单连接递增序号，当前连接接收后直接 PTY write。每次换连接/接管丢弃旧 generation 的排队字节；不自动重发断线前输入，不能承诺按键跨断线 exactly-once。服务端单一仲裁器串行处理 claim/input/resize，避免两个页面同时通过检查。接管可发生在原生审批期间；首版本机单用户不保留跨主体冻结回合的旧安全保证。

只有 controller 的尺寸驱动 PTY。resize 仅在行列实际变化时提交并合并重复值，viewer 按服务端尺寸呈现；断线保持最后尺寸。输入不等 VT 投影或数据库。屏幕恢复使用有界终端状态快照加后续输出序列；实现可复用可独立运行的 VT 代码，不能只把任意 ANSI 尾部当完整屏幕。无法恢复时标示屏幕缺口，不注入按键“修复”。原始完整 PTY 历史默认不持久化。

TerminalPanel 保持挂载，隐藏/折叠只影响布局，切换阅读 Thread 不控制 CLI。原生快捷键、IME、多行、Slash/picker 由 CLI 解释；浏览器只解决键盘传递、焦点和终端尺寸。终端 escape sequence 不自动写本机剪贴板、下载文件或打开 URL。

当前阶段已实现独立 [PTY actor](../src/workbench/terminal.rs)，将输入仲裁与写入串行处理，并以有界 VT 快照/输出订阅隔离慢消费者。原生输入缓冲区写满时，单次传递约 100ms 后返回可诊断失败；可能已有部分字节到达，输入序号已消费，页面须提示检查原生草稿，不能重放或自动补写。已接入 [WebSocket 路由](../src/workbench/web/terminal_api.rs)和独立工作台 xterm；具体检查结果及未验收范围统一见[R1 阶段证据](validation/native-cli-r1-progress-2026-09-18.md)。

屏幕重建按 [ADR 0040](decisions/0040-vt-screen-failure-isolation.md)与 CLI 生命周期隔离。已修复 `vt100` 宽字符 resize 缺陷；其他解析 unwind 会丢弃损坏状态，发出带 outputSeq 的 `screen_state_unavailable`，已连接页面继续收实时输出，输入和 resize 继续。此时重新附着只能得到不完整空快照及后续输出，页面持久提示屏幕恢复不可用；没有可靠光标时不合成 CPR。不能吞异常后假装成功恢复，也不能为了重建屏幕自动重启 CLI。

<a id="terminal-wire-contract"></a>

### 6.1 终端 WebSocket 契约

握手校验独立配对 Cookie、精确 Host/Origin、当前 runEpoch；Origin 缺失也拒绝。每次握手由 actor 分配 connectionId，消息不接受调用方指定其他连接 ID。最多 32 个终端连接，输入消息最大 96KiB，base64 解码后输入上限 64KiB；坏类型、额外字段、过期 epoch 或重复/倒序消息 ID 不可写入 PTY。

```ts
interface TerminalClientFrame {
  runEpoch: string;
  id: number;                  // 当前 WS 内严格递增，用于匹配 ack/grant/error
  command:
    | { type: "claim" }
    | { type: "takeover"; confirmed: true }
    | { type: "reconnect"; secret: string }
    | { type: "release"; generation: number }
    | { type: "input"; generation: number; sequence: number; data: string }
    | { type: "resize"; generation: number; rows: number; cols: number };
}
```

`data` 是原生输入字节的 base64，不解释为用户消息或工具命令；`sequence` 在每次 grant 的 generation 内从 1 递增，独立于 WS 消息 `id`。服务端 reply 和事件都带 runEpoch：

| type | 字段与行为 |
| --- | --- |
| `snapshot` | 首帧：connectionId、outputSeq/checkpointSeq、rows/cols、base64 screen/replay、complete/truncated、control、exit、fault；快照与注册 tail 同一 actor 步完成 |
| `output` | outputSeq 与 base64 data，必须按序；重复忽略，缺口重新握手取快照 |
| `state` | control（controllerConnection、generation、reconnectReserved、ended、rows/cols）与 exit；没有私有 secret，旧 generation 状态不能覆盖更新 grant |
| `grant` | id 和私有 grant（generation、reconnectSecret）；只回当前调用连接，不广播 |
| `ack` / `error` | 对应消息 id；input ack 只证明写入尝试成功，不表示原生提交/模型接收。失败不得自动重发；错误不回显输入正文 |
| `fault` / `snapshot_required` | 可诊断的终端异常，或订阅已超限须重新快照；不重新启动 CLI |

网页以本标签页的 sessionStorage 保存当前 runEpoch 下的重连凭证，仅页面 reload 自动恢复；新开/复制页面不使用复制来的凭证，只在终端空闲时自动 claim，已有其他页面时保持只读，避免静默接管。同页短断线可凭内存 secret 续接，成功后轮换；接管、释放和结束及时清除失效凭证。重连凭证过期时只在已确认空闲的终端上尝试 claim。浏览器未确认输入不重放，页面收到更高 generation 后立即禁用旧输入。协议保留 `release`，当前界面不提供常驻释放按钮；`takeover.confirmed` 由明确点击“在此输入”产生。

输入送达不确定与临时连接错误分开保存为 `inputUncertain`；未确认输入遇到断线、或 PTY 部分写入失败时置位，后续 `grant`、重连成功和 reload 不自动清除。sessionStorage 的 `workbench-pending-input:<epoch>` 仅存 `"1"`，绝不存输入正文或重放队列；新开/复制页面首次导航清除复制记号。用户点击“已检查终端”只清除本地提醒，不发终端帧；若另有待 ACK 输入仍保留记号。丢 ACK 后原草稿只出现一次、提醒跨重连/刷新保持的真实 Chrome 回归见[验收](validation/native-cli-r1-native-interactions-2026-09-19.md#3-已修复的不确定输入提醒)。

xterm 只在工作台挂载一次；快照恢复和 resize 都排在已有解析之后，丢弃已被快照覆盖的旧待写片段。浏览器待渲染输出有界，超过 2MiB 后重新同步；Socket 写入超过 2 秒即断开，心跳最长约 45 秒检测失联。HTTP/SSE 和模型转发不等待终端页 ACK。

正常 CSI/SGR 样式保留，包含 ANSI/真彩色前景与背景、加粗、dim、斜体以及 Unicode 表格字符；恢复快照也保留受测属性。现有过滤器限制的是 OSC 标题、链接、剪贴板等副作用与能力探测，不能为增加颜色而全量放开这些序列。固定深色 host palette 同时提供 xterm 主题和前/背景颜色探测响应；右侧仍由原生 CLI 排版，不在终端再构建一层 Markdown 或 HTML 渲染器。

工作台静态资源来自本次构建的 `web/dist/workbench.html` 及 assets，资源路径禁止向上遍历。xterm 需要 CSP 允许本页 inline style，script 仍仅允许 self；模型 Markdown 另过滤 HTML style 属性及可执行标签，不能借终端样式权限覆盖控件。

### 6.2 前端阅读状态与终端生命周期

左侧导航、中央阅读内容和终端可见性使用独立的页面状态，避免一次导航隐式改变当前 CLI。例如左侧选择 Git 时仍可阅读中央对话；点文件才切换中央内容；浏览旧运行不能切换右侧活跃 CLI。

```ts
interface WorkbenchViewState {
  sidebar: "files" | "git" | "search" | "history";
  center:
    | { kind: "conversation" }
    | { kind: "file"; path: string }
    | { kind: "diff"; path: string; scope: "working" | "staged" }
    | { kind: "search" }
    | { kind: "history"; runId: string };
  inspection: null | { scope: "live" | "history"; runEpoch: string; before?: number; requestId: string | null }; // null requestId 表示调用列表
  terminalVisible: boolean;
  followLatest: boolean;
}
```

上例包含 R4 文件/Git 的规划状态；当前实现仅提供对话、历史与调用侧面板。调用选择绑定运行及历史窗口，切换历史记录/窗口成功时关闭旧详情；历史加载失败仍保留原记录。底栏调用入口始终读取当前运行。关闭面板恢复入口焦点，入口已卸载时回到“用量与状态”；面板内 Escape 关闭，终端的 Escape 仍交给 CLI。面板覆盖的阅读内容临时 inert，节点与滚动仍保留，终端不被 inert。

该状态只表达页面阅读选择，不授予输入权，也不替代 Run、request/response 或 controller generation。`path` 仍由服务端按 §8 验证。折叠和导航仅影响展示，TerminalPanel/连接保留；隐藏后的尺寸恢复遵守本节 controller resize 规则，不通过重新 spawn 修复画面。浏览器若记忆阅读位置，恢复时只恢复可用的阅读选择，不自动执行 claim/takeover、输入或模型请求。

自动跟随只在用户已位于末尾或显式点击“跟随最新”时滚动；上滚阅读旧内容时展示新内容提示，保持阅读锚点和终端焦点。用户消息必须来自 §4 的请求/历史证据，不能把原型中本地草稿行为复制为生产消息提交机制。

底栏以 `usageSummary` 和简短保存状态服务日常使用，展开面板分别保留内容不完整与保存缺失提示；`capture`、`recorder`/持久水位和 `historyCoverage` 的技术值进入折叠诊断，不用一个绿色连接指示灯代表三者。只读/接管提示属于终端输入权区域，不能覆盖为一张猜测的原生审批卡。UI 示例、截图和静态 mock 的成功态不进入运行时状态判断。

## 7. 异步记录、索引和旧数据

### 7.1 文件布局与保存语义

本节是 R3 实现契约；关键决定见 [ADR 0044](decisions/0044-workbench-asynchronous-history.md)。默认目录按 [ADR 0049](decisions/0049-workbench-user-directory.md) 更新为 `~/.codex-web/history`，Windows 对应 `%USERPROFILE%\.codex-web\history`，独立于原生 `CODEX_HOME`。`--data-dir` 可指定其他私有目录，其父目录须已存在；默认根可由异步记录器创建。旧目录不搬迁、合并或自动选取，需显式指定后继续读取。

```text
<workbench-data>/runs/<runEpoch>/
  lock
  meta.json
  observations.<segment-uuid>.jsonl
  snapshot.json
  blobs/<blake3-content-id>
<workbench-data>/index.sqlite
```

目录 0700、文件 0600。journal 是已脱敏的公共 ViewEvent、上下文文档及来源关系，不保存终端输入、完整 PTY、认证头或私有工具参数指纹。文件/子目录通过目录描述符访问并校验所有者、类型和权限；拒绝符号链接、硬链接普通文件及非法 blob ID。SQLite 索引只由内部随机临时文件构建，不读取未知索引中的 SQL。

记录使用格式版本 1、runEpoch、recordSeq、viewSeq、kind、data 与 blake3 摘要。recordSeq 包含上下文文档，不能当作输入序列或 viewSeq。单行含换行最大 8MiB；大于 64KiB 的 data 使用 blob，blob 和分段基线最大 128MiB。`meta.json` 最大 4MiB、最多 4096 个分段，达到上限进入保存失败，不发布无法恢复的新水位。读取遇到未知版本会明确不可用，不套用旧格式猜测。

Recorder 独立线程接收最多 256 包/8MiB 的队列，生产者只用 `try_send`。每批最多 256 包或 256KiB，空闲等待 100ms，默认每秒及退出时提交。顺序是 blob 同步 → journal append/fsync → 原子替换并同步 meta/父目录 → 发布水位。meta 记录每段已提交字节数及序列；只在成功完成屏障后推进 `persistedThroughViewSeq`。正常结束另写可重建的 `snapshot.json`，当前读取仍验证 journal 和分段基线，不信任该派生快照或索引来补正文。

退出最多等待 Recorder 2 秒，超时发布 `shutdown_timeout` 并分离线程；最终提交在 journal fsync 后再检查退出许可。没有确认最终提交的历史标未正常结束；已进入内核的 I/O 无法取消，超时不是磁盘不再写入的保证。只有完整提交标记以内的内容可恢复，异常退出可能丢失曾显示的尾部，不保证最多只丢 1 秒，也不虚构丢失数量。

队列满/写入或同步失败冻结连续水位，终端和实时阅读继续。约每秒尝试恢复：写新 segment、明确 view/record 缺口区间、当前内存快照。`savedThroughViewSeq` 可推进，连续水位不能跳过缺口。缓存淘汰用 `view_reset` 同时保留边界快照与触发事件；它不是保存故障，原 journal 仍可向前翻阅。完整但超出提交字节数的 JSON 行同样不作为已保存内容。半行、坏摘要、缺失日志、基线损坏、水位不符均报告 segment/byteOffset；前段损坏后可显示后段独立快照，但已验证连续水位不再推进。

### 7.2 历史查询与 Rollout 补齐

历史请求使用独立工作线程和最多 8 项的查询队列，响应等待上限 3 秒。HTTP handler 不执行同步磁盘遍历，既不扫描原生 home，也不阻塞转发。运行列表从最多 10000 份提交元数据构建可重建 SQLite 索引，按 startedAt/epoch 倒序每页 20 项；索引损坏/删除会重建，索引写入失败可从同份已验证元数据返回列表并标 unavailable。超过运行数量界限明确不可用，不静默截断。该首版逐次重建策略不承诺大库检索性能。

列表返回 `{ currentRunEpoch, runs, nextCursor, diagnostics, indexState }`。游标绑定当前有序运行集合，新增/删除运行时旧游标返回 409，非法游标返回 400。列表与读取按 canonical cwd 摘要隔离，其他项目的 Run 按 404 处理。运行状态由 `lock` 的 flock 加已提交结束标记判断，不依据 PID 猜服务存活。

单次读取返回 `{ run, snapshot, gaps, issues, uncertainTail, detailsPartial, before, previousBefore, source: "saved_workbench", currentRunEpoch }`。`snapshot` 复用 schemaVersion 2，连续水位取提交元数据与实际验证前缀的较小值。`previousBefore` 只在存在更早阅读窗口时提供；`before` 读取该位置之前（含该位置）的保存状态。上下文同样绑定历史 Run、request、recordSeq 和可选 before，每页最多 16 项/128KiB；缺失、策略省略、截断和保存缺口分别返回。无配对返回 401，未保存/跨项目返回 404，繁忙或记录不可读返回 503；均保留既有 Host/Origin 保护，历史正文不可编辑。R3.1 的管理写接口、读取失效/410 和游标冲突处理另见 §7.4。

历史页面复用角色气泡、工具结果、Markdown 与调用详情，提供“查看此历史的调用记录”，详情始终携带选中的 runEpoch/before。历史面板不再复制第二份网络对话，并显示“保存时”状态、观察缺口、损坏位置和异常退出尾部不确定性。更早窗口/更多运行按需读取；失败保留现有阅读选择。切历史保留同一当前 TerminalPanel；返回实时阅读保留此前面板、展开与滚动位置。不自动 resume、不重发旧输入，也不合并旧 Run 到当前聊天。

RolloutReader 后台只读 `sessions` 和 `archived_sessions`，以明确 thread/turn 关联当前 Run；文件通知只是重扫提示。保留原始 sourceRef/偏移和文件身份，归档移动沿用身份去重，坏行不阻塞其他文件，退出半行可诊断。来源变化无法验证时保留缺口，不按时间或相同文本重建关联。工作台历史保存的是当时有证据的观察副本；它不导入所有原生旧会话，不直接读原生 SQLite，也不以 rollout 最终结果冒充丢失的网络中间 delta。

### 7.3 旧项目迁移

旧 schema 20、raw/audit 数据原样保留，不作为新数据库 migration 的前提。独立历史 adapter 可读取旧 `/v1` 或脱敏导出；旧 eventSeq 不重编号成新 captureSeq。新 `/workbench/v1` 不承诺兼容旧 `/v2` mutation。R5 已先验证导出/历史能力和新运行闭环，再按用户授权删除旧控制路由与代码；`/v2` 返回 404，旧控制恢复和图片清理不再运行。V1 网页更新使用 `/v1/stream` 的 `afterEventSeq`，过期后重取快照水位。未修改用户服务配置或数据库。

### 7.4 历史管理与统一配置扩展

2026-09-20 用户要求加入占用、按运行删除、批量清理、可选保留期限及 JSON/网页配置。详细契约独立在[历史与配置方案](codex-native-cli-workbench-history-settings.md)，为本文组成部分；此处不重复维护接口/字段，实施与验收归 R3.1。

核心边界：后台扫描逻辑字节数并表达 partial；默认不允许手动或自动删除；精确 UUID 预览后确认，执行前复核当前项目、配置和运行 flock；整次运行移入同数据根暂存后可恢复地删除。新管理路径不等待/阻塞转发，也不修改原生会话、旧库或当前 Token/保存水位。仅移入暂存不表示空间已释放。

现有 meta v1 严格拒绝未知字段。保留期限使用新增独立 `lifecycle.json` 绑定最终已提交 meta 摘要和 endedAt；不迁移或补造旧记录时间，缺凭证/异常记录跳过自动清理。工作台配置默认在 `~/.codex-web/config/config.json`，Windows 对应 `%USERPROFILE%\.codex-web\config\config.json`；网页与本地文件共享契约，独立于原生 `CODEX_HOME`。路径/启动项下次启动生效，当前数据根不变。网页只在当前工作台点击展开配置面板，保留阅读选择与终端；没有独立设置页或前端路由，settings 仅为数据 API。决定与兼容取舍见 [ADR 0048](decisions/0048-history-cleanup-and-json-settings.md) 及其默认路径补充 [ADR 0049](decisions/0049-workbench-user-directory.md)。

## 8. 文件、搜索与 Git 阅读

```text
GET /workbench/v1/workspace/files?path=<relative>&cursor=<opaque>
GET /workbench/v1/workspace/file?path=<relative>
GET /workbench/v1/workspace/search?q=<text>&regex=false&caseSensitive=false
GET /workbench/v1/workspace/git/status
GET /workbench/v1/workspace/git/diff?scope=working|staged&path=<relative>
GET /workbench/v1/workspace/git/log?cursor=<opaque>
```

R4 实现为独立 WorkspaceReader 线程、8 项有界队列和每请求 5 秒预算（含排队），不与代理、PTY 或历史查询共用工作队列。只读根取 Launcher 确认的 canonical cwd，绑定启动时目录描述符；浏览器不能传 executable/env/root。`/run.workspaceRoot` 只供当前本机工具路径归属判断，根外及远程环境路径不生成文件打开动作。

冻结预算：目录每页 200 项，最多扫描 10,000 项/1 MiB 文件名；文件最多 1 MiB；搜索最多 200 命中、2048 文件/32 MiB 安全文本，5 秒截止；Git stdout 最多 1 MiB、5 秒，log 每页 20 条。前端文件/Diff 使用单文档连续滚动和视口虚拟渲染，长行横向滚动，不再按行数或字符切段；Git 变更列表每组每页 200 项，超限明确提示。UTF-8/NUL 检测拒绝二进制文件。

成功响应均含 `currentRunEpoch`、`readAt`（完成读取的 UTC 时间）、`elapsedMs` 和 `truncated`。具体契约：

| 接口 | 额外结果字段 |
| --- | --- |
| files | `path, entries:[{name,path,kind:file\|directory\|unavailable}], omitted, nextCursor`；无 path 表示根；游标绑定 path/条目摘要 |
| file | `path, text, revision`；revision 是读取内容摘要 |
| search | `hits:[{path,line,text,truncated}], scannedFiles, omitted, limitReason`；行号从 1 起，片段最多 1000 字符；literal 为默认 |
| git/status | `branch, entries:[{path,index,working,originalPath}], omitted`；branch 为 null 时分离 HEAD，两个状态使用 porcelain 字符 |
| git/log | `commits:[{id,shortId,author,date,subject}], nextCursor`；游标绑定 HEAD 与项目 prefix，仅列影响当前目录的提交 |
| git/diff | `path, scope, patch, revision, comparison:"file_contents", empty`；patch 仅含 hunk，不是可应用/可导出的完整 Git patch |

所有接口复用配对 Cookie 与 Host/Origin 防护，只注册 GET。未知查询字段、非法正则/游标返回 400，路径/敏感文件/链接拒绝 403，文件不存在或无 Git 仓库 404，集合变化/并发文件修改/冲突 409，文件/目录/Git 超限 413，二进制 415，能力缺失/繁忙/I/O 503，截止 504；错误含 source 和 currentRunEpoch，不带正文或根外路径。没有 Git/rg 不影响 CLI、对话及其他阅读能力。

逐组件 `openat(O_NOFOLLOW)` 阻止路径穿越与替换为 symlink；根内 symlink、普通文件硬链接、设备/FIFO 也拒绝。`.git`、常见凭证目录/文件名和私钥名按统一规则排除；这不是对任意源码内秘密的检测。目录扫描和文件读取检查前后元数据，异常明确返回。目录名称按 UTF-8 展示，不可编码名称省略。

rg 固定 argv、`-e`、JSON 和项目 ignore 规则；先枚举候选，再安全读取并以最多 16 个匿名文件描述符一批交给 rg，不执行 shell/preprocess，不让 rg 重新打开项目正文路径。Git 固定 cwd/argv、`-z` 路径、当前目录 pathspec/prefix 双重范围，禁用 pager、external diff/textconv、fsmonitor、hooks、可选锁、子模块递归、签名与远程抓取。Diff 从验证过的 HEAD/index blob 与安全读取的当前文本生成匿名快照后比较；因此模式/rename 看左侧状态，冲突不伪造单一版本。决定、边界与替代方案见 [ADR 0050](decisions/0050-read-only-workspace.md)。

只刷新当前可见查询，每次完成后至少等待 5 秒；文件树保留展开项但只轮询最后选中的目录。其他展开项保留上次结果，重新选中或“刷新文件”可更新。首版使用有界轮询而不增加递归 watcher，外部文件变化与已确认工具结果由下一轮读取反映，不把通知当作 Git 状态。切换/收起取消未完成查询，取消/超时回收查询子进程，已进入内核的磁盘 I/O 不能保证立即终止。

文件树、搜索、Git 面板只改变左侧，中央可继续显示对话/历史。打开文件、搜索命中或有来源的工具路径才改变中央内容；代码行号、Diff 区分和读取时间保留，历史工具链接标明阅读当前文件。关闭文件回到此前中央选择。没有文件写、Git 写、命令执行或切换 CLI 的浏览器接口，也不增加 Git 锁与工作区独占承诺。实际结果见 [R4 验收](validation/native-cli-r4-acceptance-2026-09-20.md)。

2026-09-22 文件阅读按 [ADR 0052](decisions/0052-continuous-file-reading.md) 改为按需加载的 CodeMirror 6 只读视图：行号、滚动条、跨视口选区复制、全文查找与行号跳转共用完整的当前文档；Diff 保留源行按钮和颜色。一个中央文档实例，关闭/切换即销毁，不建逐文件缓存。相同 revision 不重置模型/选区/滚动，变化时按公共前后缀替换差异并映射位置。视口附近的行才创建 DOM，Diff 另有每行 4 字节的源行映射；1 MiB 输入边界不等同于整个浏览器堆上限，实际测量见[连续阅读验证](validation/native-cli-continuous-files-2026-09-22.md)。不加入编辑、保存、语言服务或自动加载外部资源。

侧栏展示按 [2026-09-21 试用反馈](validation/native-cli-r4-ui-2026-09-21.md)调整：活动栏统一静态 SVG 和中文短标签；文件树用目录/文件类型图标、缩进引导线、单行省略及当前文件选中态；Git 文件名、目录、原路径和中文状态分别展示，变更按 `path + scope` 高亮，保留完整路径的可访问名称和悬停提示。只将中央实际打开的 `FileSelection` 传给侧栏，不另建选中数据源或新增读取请求。选中目录的 `readAt/truncated` 移至树外底部，其他目录仍保留各自上次结果；Git 同样保留读取时刻和截断，解释文字折叠。已有结果的刷新不插入加载行，失败在结果上方说明仍是上次成功读取的内容。显示调整不改变查询、轮询、取消、分页或终端生命周期。

图标进一步按用户要求采用 Microsoft Codicons 固定提交的 20 项本地 SVG 子集，映射、来源和许可见 [icons/README](../web/src/workbench/icons/README.md)。原始 `viewBox` 和路径保留，由 Preact 创建受控 SVG 节点；不注入 SVG 字符串、不加载字体或远程资源。中文名称、键盘焦点与树状态仍由原控件提供，装饰图标对辅助技术隐藏。[图标验收](validation/native-cli-r4-codicons-2026-09-21.md)单独记录此次展示变更。

## 9. 性能测量与验收

### 9.1 对比方法

优先比较普通 CLI 直连与新模型代理，固定同一官方 CLI、合成流内容/频率/大小和本机环境。main 旧代理只作可选参照，不为取得旧指标扩展旧内核。捕获层不同，模型 chunk 到 CLI 的阶段与 App Server frame 阶段必须分别标注，不能将不相同的测点直接相减。

网络记录 monotonic 时间：接收片段、转发提交、对端收到、进入 reducer、发送 Web、页面应用及前台 paint。服务端不能直接减浏览器的 `performance.now()`；本机联合探针建立时钟映射并报告误差，或以同一探针观测两端。DOM commit 与首次实际可见也应分开。若测旧 writer，须包含 enqueue/凑批等待，不能只报告事务耗时。

初始目标：额外转发 p95 ≤ 5ms，接收至前台页面可见 p95 ≤ 100ms，非全场景承诺。已有[合成 HTTP 客户端与 Chrome DOM/Paint 测量](validation/native-cli-r0-progress-2026-09-18.md#25-合成转发dom-和主帧-paint-测量)及[普通 CLI 对照](validation/native-cli-r0-progress-2026-09-18.md#28-普通-cli-的固定流直连代理对照)。正常案例代理接收→合成客户端 p95 ≤0.129ms、接收→Chrome 主帧 Paint p95 ≤74.092ms；CLI 对照覆盖 100Hz 短/长输出和观察暂停，PTY 测点包含原生渲染，不当作网络开销。R3 另验证 Recorder 停顿 5 秒、满队列、保存错误和 Chrome 故障恢复；测得的 HTTP 客户端往返不是额外代理开销。完整 Web 阶段分解和产品资源矩阵仍由 R5 验收。至少覆盖短文本、100Hz 小片段、长输出、并发子请求、慢网页、Recorder 阻塞 5 秒和磁盘错误。报告 p50/p95/p99、样本数、机器/CLI/browser/profile、丢观察数、内存峰值、CPU、TTFT 与整轮时间。真实模型只作联合行为验证，不能靠随机网络耗时下结论。

### 9.2 必测矩阵

| 组 | 验收结果 | 切片 |
| --- | --- | --- |
| P01 接入 | 普通 CLI 进程、无本项目外部 App Server；配置/认证不污染，支持 profile 明确 | R0 |
| P02 转发 | SSE/WS、多轮/取消/断线、压缩/UTF-8/未知事件；应用字节一致，无额外重试 | R0/R1 |
| P03 终端 | 中文/多行/picker/信任/原生审批；双页面单写、接管、resize 不循环、切面板不 spawn | R1 |
| P04 语义 | 按 §4.3–4.5 区分用户/模型、请求用途与工具类型；request/response/stream/call 不串联；工具生成不等于执行、response 完成不等于 Turn 完成；逐项见[聊天验收](v2-implementation-plan.md#chat-acceptance) | R1 角色与用途 / R2 工具与复杂上下文 |
| P05 阅读 | 首片可见、最终替换、重复/乱序/旧 revision、上滚不抢焦点；稳定气泡/工具卡原位更新，历史上下文不重复、相同新输入不丢失 | R1/R2 |
| P06 历史 | snapshot/replay 无竞态；缺口、重启未保存尾部、磁盘满与索引重建均可诊断 | R3 |
| P07 隔离 | 慢 recorder/viewer/解析异常不阻塞 CLI；有界内存，观察丢失不丢网络字节 | R0/R3 |
| P08 安全 | 错误 Origin/Host、开放代理、重定向、secret、HTML/ANSI 和路径越界负向测试 | 各相关片 |
| P09 工作区 | 特殊路径/嵌套目录/无 Git/rg、取消与并发文件变化 | R4 |
| P10 实机与退役 | 系统 Chrome + 官方安装版 + 实际模型，图片/真机结果、旧历史保留和旧控制退役 | R5 |
| P11 前端原型对照 | 按[实施计划 §4.1](v2-implementation-plan.md#prototype-acceptance)及[§4.2 聊天验收](v2-implementation-plan.md#chat-acceptance)逐条操作；对照方案 §1 的深色布局、角色标签、用户/模型气泡、工具/命令/结果卡和请求入口；左侧 Git 与中央对话共存；折叠/导航保持终端，历史不切换 CLI，捕获/保存/覆盖与接管分别呈现；键盘可达、窄屏无横向溢出 | R1–R4，真机归 P10 |
| P12 历史管理 | 占用/partial/分页、精确候选、默认关闭删除、活动锁/跨项目/链接/路径替换负向、批量/幂等/读删并发、各删除崩溃点及暂存恢复；按可靠结束时间保留，旧/异常记录不自动删除；慢 I/O 与原生/旧数据不变 | R3.1，详见[验证契约](codex-native-cli-workbench-history-settings.md#7-验证限制与后续落点) |
| P13 统一配置 | JSON/Schema/默认/参数覆盖、字段与路径校验、原子保存/备份失败/修订冲突、外部编辑/损坏时禁删、共享范围/下次启动；点击展开/收起及草稿保留、文件往返、焦点/终端/阅读位置保留，无独立设置页或前端路由 | R3.1，同上 |
| P14 设备接入 | 同一服务 IP/MagicDNS 同时可用、切换二维码不改变连接、共用 Run 级固定配对码（可重复扫码、重启更换）与浏览器授权、Host/Origin 对应、撤销流/重连/排队输入、默认关闭、HTTP 浏览器兼容与真机操作；不依赖 Serve | R5，见[设备接入设计](codex-native-cli-workbench-device-access.md)与[实施顺序](v2-implementation-plan.md#device-access-slices) |

所有 fixture 合成或脱敏，不写真实用户 home。没有性能测量、安装版接入成功和 P07 故障证据，不能把本设计标为已实现；cc-viewer 的 T01–T13 不能替代本项目验收。
