# Codex 工作台开发核心约束

> 当前方向：2026-09-18 用户授权完全重构，按 cc-viewer 的目录启动、普通 CLI、模型代理、实时直推与异步历史实现。
> 状态：设计已更新；实现进度见 [V2 实施计划](v2-implementation-plan.md)。当前 main 仍是旧 Session Runtime，不能把目标写成已实现。
> 决策：[ADR 0039](decisions/0039-cc-viewer-style-runtime.md)；[整体方案](codex-native-cli-workbench.md)；[详细设计](codex-native-cli-workbench-detailed-design.md)。

## 1. 文档目的与约束级别

本文约束新工作台设计、实现、测试和迁移。当前会话明确要求优先于 AGENTS.md，后者优先于本文与详细设计；旧代码不是必须复用的约束。

旧 Controller/Session Kernel 设计、分片与重复约束已移除，历史事实通过[旧 V2 索引](archive/v2-before-cc-viewer.md)追溯。ADR 0038 的 raw-first 增量草案已撤回；本文不延续其 N1–N5。实现状态只在 [V2 实施计划](v2-implementation-plan.md)维护，不通过改文档伪造实现或验收。

## 2. 已确认事实与产品决定

2026-09-18 事实基线：main `930172493c163a903621ca14c540d1ca10dd4f3e`，Observer schema 20；cc-viewer 源码 `5b544c6ee112bb14a80670480d3c4c900b17d307`、实际运行 1.8.18；官方 Codex 研究源码 `633ab199cfd724aa78013c006b27a2b3d049fc3b`、当时安装版 0.154.0。ccv 试用与本项目 R0 实验分别记录；本项目已执行的合成接入、网页和隔离检查见[阶段证据](validation/native-cli-r0-progress-2026-09-18.md)，不能据此推导真实 provider 兼容或已满足性能门槛。2026-09-20 安装版已升级为 0.155.1，新增验证见[版本回归](validation/native-cli-version-compatibility-2026-09-20.md)。

新主线是本机原生 CLI 工作台，V1 历史读取价值继续保留；旧 V2 控制内核可以被替换，不再要求保留 ledger/CAS/lease 或旧 `/v2` mutation 兼容。V3 IM 目标保留为未来产品，其原架构依赖需在未来重新设计，不提前混入本轮。

官方事实、参考源码、项目选择和待验证假设必须分开。绝对源码路径只用于研究，不作为运行依赖；官方 CLI 不打补丁、不自动升级或替换。

## 3. 产品目标与非目标

从项目 cwd 运行拟定 `codex-view`，打开本机网页和普通官方 CLI。右侧真实终端承担全部对话、Slash、信任、权限和追问；中央实时阅读，左侧文件/搜索/Git，另有模型请求上下文诊断。

每次启动一个项目、一个交互 CLI；多个页面一个可输入终端、其余可读。网页不另造 Composer、网页队列、审批表单或原生模型/权限设置状态机。2026-09-20 用户要求增加工作台自己的 JSON 配置，网页只在当前工作台点击展开可视化修改面板，不新增独立设置页面或前端路由，属于 R3.1，不接管 Codex 原生配置。首版不接管外部 CLI，不做网页项目文件编辑、任意 Shell、开发端口代理、远程多用户或 IM。图片和手机能力须独立验证。

## 4. 运行与代理约束

- 直接启动普通官方 CLI，不启动本项目管理的外部 App Server，不使用 remote TUI。官方 CLI 内部实现由官方负责。
- CLI 版本号不作启动白名单；用户升级后照常尝试运行，未回归版本只提示。版本探测和路由/认证错误分别诊断，不通过版本差异推断不兼容；当前证据见 [ADR 0045](decisions/0045-cli-version-compatibility.md)及[版本回归](validation/native-cli-version-compatibility-2026-09-20.md)。
- 在原模型服务前放显式本机 HTTP/SSE/WS 代理；允许这一经用户授权的新路线，旧模型 API 代理禁令不再适用。
- Launcher 对模型接入只覆盖本次运行经过验证的路由，不改全局配置、原 provider/模型/权限或真实 CODEX_HOME。2026-09-19 用户确认的显示例外：网页终端使用 truecolor、子进程清除继承的 `NO_COLOR`，并用官方 `tui.animations=false` 关闭装饰动画；不靠删改输出字节移除动画。
- 不假定所有认证模式和配置优先级都兼容；R0 先验证当前必要 profile，其他 profile 验收后加入支持清单。无法确认 upstream 时明确失败，不猜服务，不强制关闭 WS，不静默切回旧内核。
- 网络转发和 PTY 不等待观察解析、脱敏投影、日志、数据库、网页 fetch 或 ACK。普通网络背压与观察背压分开。
- 代理不新增模型重试、不重放未知结果请求；unknown 观察不影响原始合法数据转发。

## 5. 认证与安全

默认启动仅 loopback；Web 配对认证、Host/Origin/CSRF、模型代理运行期能力和固定 upstream 必须存在。模型代理与浏览器 API 不共用认证入口，不提供任意 upstream、CONNECT 或开放 CORS。用户 2026-09-21 要求同一使用者通过 LAN IP/Tailscale MagicDNS 接入，已实现的显式接入例外仅限网页入口，见[设备接入设计](codex-native-cli-workbench-device-access.md)与 [ADR 0051](decisions/0051-workbench-device-access.md)；按本次实例显式开启，共用 IP/MagicDNS 监听与浏览器授权，模型代理仍只能 loopback。自动化/Chrome 验证与手机真机验收分开记录，不自动修改防火墙或 Tailscale。

认证材料只在必要转发链路中出现，观察副本在进入 UI/持久化前脱敏；不要把 bearer、Cookie、URL token、加密 reasoning、图片 base64 或私人 fixture 写入仓库/通用日志。上游 TLS 正常校验，不安装本机根证书做系统 MITM。

单写权是内存连接仲裁，日常界面不显示启用/释放按钮。空闲终端随页面就绪自动可输入；只有多页面冲突时显示“在此输入”，点击一次切换，无二次确认。输出始终可读。原页面可短时重连，可信本机用户可明确接管；不保留旧持久 InputLease/TurnOwner 的跨主体保护承诺。未来远程功能必须重新设计授权，不能把这个首版模型直接外推。CLI 的原生审批和 sandbox 不被代理跳过。

## 6. cwd 与执行边界

Launcher 确认的 canonical cwd 是文件/搜索/Git 的根，不由浏览器任意扩大。只读工具使用固定命令与参数、路径/敏感文件检查、取消和输出限制；防止 symlink/TOCTOU 越界。不同项目另行启动，不借管理终端暴露通用 Shell。

Git 视图不意味着工作区独占，不增加 Git 锁或替其他进程协调写入。Git/文件写功能只有明确后续范围与验收后才纳入。

## 7. 输入与语义

原生 CLI 是唯一对话输入和原生交互状态机。按键直接送 PTY；Enter、终端文本、光标和最近通知都不能证明已提交消息、当前 Thread 或任务完成。工作台配置表单只保存本产品选项，不向终端发送命令或按键。

模型 response、request、wire Item、Codex Thread/Turn 是不同身份。正文依据模型实际 delta 展示；工具参数生成不等于执行，response completed 不等于整个 Turn 完成。工具结果和任务状态可由后续请求或明确关联的 rollout 补齐；没有证据就标未知。

网页阅读 Thread 的选择不控制原生焦点。恢复页面不自动发送、resume、回答审批或重放输入。

## 8. 图片与设备

先验证安装版 CLI 在网页终端中的真实图片接纳路径与中文/软键盘行为。不把文件路径或 xterm 文字粘贴当成图片已送达；不恢复旧网页上传/queue 状态机作为默认。必要最小桥接单独给出来源、清理和原生确认契约，不自动 Enter。

## 9. API 边界

新接口为 `/workbench/v1`，具体契约见详细设计。实时事件直接携带内容/patch，正文不使用通知后每帧 GET。历史和大请求详情可按需读取。

旧 `/v1` 保持只读，旧数据库可以通过独立历史 adapter 读取；它们不成为实时运行依赖。新 API 不承诺旧 `/v2` command/lease/owner 契约。旧生产服务行为在真正实施迁移前保持原状，文档变更不等于配置已切换。

## 10. 存储、事件与恢复

- 转发链路仅 tee 有界观察副本；内存 reducer 和 push 不等待磁盘。
- 后台 journal 保存脱敏观察/来源与格式版本，索引可重建；不绑定 Codex 私有 SQLite 表。
- 仅在实际 fsync 后推进连续持久水位，原始内存序列、页面序列和磁盘水位不混用。
- 记录队列满、磁盘错误、解码不支持或慢消费者均显式标记；记录/阅读失败不主动关闭模型连接。
- 已显示未保存尾部可以丢失；snapshot、rollout 最终内容不能假装恢复全部中间事件。区分 live capture 状态、保存状态和历史覆盖，旧单一 completeness 枚举不强制复用。
- 服务重启保留已存历史，旧 Run 标结束/异常；不自动复活 CLI、接管外部进程或重放输入。
- 当前持久化契约见 [ADR 0044](decisions/0044-workbench-asynchronous-history.md)：独立 Recorder、提交标记以内的恢复、连续水位与恢复分段水位分开，历史只读且不恢复 CLI。
- 新数据目录独立，旧 schema 20/audit 保留；未经迁移验收不删除、覆盖或自动改写用户数据。

后续历史管理按 [ADR 0048](decisions/0048-history-cleanup-and-json-settings.md)及[配置/清理契约](codex-native-cli-workbench-history-settings.md)实施：默认关闭所有历史删除；占用、手动/批量删除和按天保留均限当前项目工作台数据。活动运行锁/身份必须重验，自动策略跳过旧记录的未知结束时间；删除任务需可恢复且不阻塞转发。JSON 与网页表单读写同一配置，损坏/未知配置暂停删除，启动和路径选项下次生效。原生/旧数据保护不因新增清理能力而放松，历史正文仍只读；R3.1 管理 mutation 不进入旧 `/v1`。

## 11. Web 交互

终端与阅读持续并列，切换中央面板不卸载原生进程。正文、工具和请求上下文安全渲染；未闭合 Markdown、HTML、ANSI、SVG 均按不可信处理。用户上滚、展开内容或读文件不抢回滚动/输入焦点。

网页明确区分模型响应完成、执行结果未观察、捕获缺口、保存异常和终端断开；不能用一个绿色“已连接”掩盖其他路径失效。具体 source/protocol 等实现细节放诊断页，主流程用用户能理解的状态。

## 12. 实施顺序

以 [V2 实施计划](v2-implementation-plan.md)的唯一状态表、任务与通过条件为准。顺序为模型代理闭环 → 终端与实时工作台 → 工具/请求阅读 → 历史恢复 → 历史管理与统一配置 → 工作区 → 完整验收与旧内核退役。

先证明无同步持久化屏障的一轮原生对话，不先重建通用控制平台。R0 若否定必要 provider 的接入假设，先用实验事实修订 ADR，再实现其他路线；不因旧架构已存在自动回退。

## 13. 测试与发布门槛

R0 必须记录安装版、profile、cwd 与真实中间态，比较普通 CLI 直连与新模型代理；旧链路可作辅助参照，须注明不同测点。5ms 转发/100ms 前台展示 p95 是初始目标，不是已测结果。慢 Recorder、观察队列满、慢网页和 decode 失败不阻塞 CLI 必须有证据。

保持中文/多行/Slash/picker、原生审批/追问、单写/接管、SSE/WS、snapshot/replay、缺口、磁盘满、crash、unknown、secret/Origin/路径负向测试。用临时 CODEX_HOME、合成项目和系统 Chrome；不下载额外浏览器。代码变更按 formatter/lint/typecheck 和相关测试验收，文档变更检查链接、术语和示例。

## 14. 迁移与兼容

新入口达到验收后才能切换用户服务并退役旧控制模块。旧历史可读、数据可导出，兼容 adapter 不调用旧控制面。回退时先明确结束新 Run，不双重控制。不得以 full refactor 授权推导生产数据库删除、CLI 升级或远程发布授权。

## 15. 变更与交付

设计和代码状态分别报告；关键选择更新 ADR、实施计划中的需求追踪、能力支持矩阵和验收。文档删除与重新规划不等于代码退役、配置切换或 migration。R0 的成功或失败都要可复现，不能只写“更接近 cc-viewer”或“理论更快”。
