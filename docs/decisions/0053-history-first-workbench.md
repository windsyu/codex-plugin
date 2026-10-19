# ADR 0053：历史首页、按项目启动与旧历史只读整合

- Status：Accepted；独立设置页和后台服务分别由 ADR 0059/0063 扩展。
- Date：2026-09-22

## Context

R0–R5 将普通官方 CLI、PTY、模型代理和工作台直接绑定为一次启动。旧 V1 跨项目历史保留在独立 `codex-observerd`，新 `codex-view` 页面没有入口，且当前迁移上限 20 无法直接启动使用 schema 25 的实际旧库。只保留代码和 schema 20 fixture 没有完成用户的历史阅读闭环。

用户要求将这些能力整合为 R6，倾向默认先看到全部历史，然后进入已有项目或指定新路径启动 CLI；并明确确认：另一个项目在新工作台标签页启动，保留原项目；项目入口默认新对话，历史详情另有“继续此会话”。

只读表结构对照发现，实际 schema 25 保留 schema 20 的核心历史表列，新增内容主要是旧控制队列/上传表；这支持独立 reader 方案，但不证明正文语义与 WAL 兼容。源码、实际结构核查与证据限制详见 [R6 设计](../design/history-home.md)。

## Decision

采用以下决定；后续入口扩展和持续限制见 Consequences：

1. `codex-view` 默认启动 Application 与全部历史首页，零 CLI/PTY/模型代理；CLI 安装/profile 校验延后到明确开始工作。保留显式目录/会话直达入口，不自动恢复最近任务。
2. Application 拥有同源网页、配对、配置和历史目录；独立 WorkbenchRuntime 拥有一个项目的原生 CLI/PTY/Proxy/Recorder。内存 RunRegistry 隔离并行项目，同项目重复点击进入已有 Run。它不是持久控制账本，不恢复旧 Worker/App Server。
3. 历史读取由工作台记录、原生 rollout、旧 Observer SQLite 三类 adapter 提供；后台有界索引/脱敏搜索与按需正文读取独立于实时热路径。前端统一项目目录与阅读样式，保留每条来源和能力，不把多个来源拼成虚假的完整会话。
4. 旧库原位只读，初始验证 schema 20 与实际 schema 25 的读契约。禁止 migrate/import/rebuild/purge、修改 user_version、恢复旧控制表行为、整库复制或对活动 WAL 使用 immutable。只读 SQLite/VFS 与辅助文件零写的可行性先用合成数据验证。
5. 全局缓存和配置保存在 `.codex-web` 的既有目录体系，索引可重建。新 Run 补充项目路径 sidecar；旧 Run 无路径时不猜。来源登记和设置原为当前页展开面板，后由 [ADR 0059](0059-workbench-routes-and-relay-recovery.md)改为独立页面。
6. 目录启动是本机所有者明确操作，目标路径/身份/配置修订需重验。会话恢复使用可信 SessionMeta.id 和对应 nativeHome；点击历史只读，不向 CLI 重放正文/Enter。
7. 跨项目可读不扩大原清理范围或已有手机授权。手机配对绑定 Run，新旧路由均校验目标；其它项目历史、全局来源、启动管理对现有手机授权不可见。模型代理仍仅 loopback。
8. 保留前台运行；同配置/数据根的重复启动通过私有实例发现复用应用。后由 [ADR 0063](0063-background-service-directory-browser.md)增加显式 macOS 用户级后台服务，默认不登录自启。退出仅回收本应用的 Run；首页或其它 Run 不因某一项目停止而退出。

## Alternatives

- 在新页面放链接或 iframe 到 `codex-observerd`：仍要求第二服务、端口、配对和数据库写入初始化，无法解决默认入口与 schema 25 的实际问题。
- 首页仍自动启动 cwd CLI，只把中央切为全部历史：每次只读访问仍创建子进程，CLI/provider 不可用会挡住历史，不满足用户选择。
- 把旧 schema 上限改为 25 或将数据库降回 20：没有验证读写兼容，可能运行旧回填/清理，不解决生命周期和入口问题。
- 从原生 rollout 重建新库，丢弃旧 Observer 来源：会丢失原生文件已不存在但旧库仍保留的可读记录。
- 每个项目启动另一个 codex-view/observer HTTP 服务：改装较少，但端口、手机配对、来源管理和退出回收再次分散。
- 停止当前项目再切换或默认续最近会话：用户已选择保留并行项目与默认新对话。
- 直接引入持久多会话控制系统：超出本机项目运行需要，也会恢复已退役的控制复杂度。

## Consequences

无参数启动语义、应用入口文件格式、内部工作台 API 的 Run 寻址会改变；须更新 README、前端所有 HTTP/SSE/WS 客户端、运行/认证/刷新/退出测试。当前新工作台并非仅需要加一个“全部历史”按钮。

允许多个项目并行是用户授权的 R6 增量，替代“一个启动进程固定一个项目”的产品边界；每个 Run 仍只有一个普通 CLI、一套 PTY 和明确的目录/身份。V3、多用户与任意 Shell 仍排除。

新目录缓存拥有独立 schema，统一 JSON 在 R6-C 升级为 2，schema 1 只读加载不自动落盘，用户保存时备份后写新格式。原生数据、旧 Observer schema/audit、现有 Run 的 journal/meta 均不自动迁移或删除；现有 `codex-observerd` 写入口仍受旧版本保护。

旧库使用专用只读 VFS 与 `readonly_shm`，保留 SQLite 锁和一致性协议；需要补建或恢复辅助文件时返回不可用，不用 `immutable` 绕过。兼容规则见[旧 Observer 契约](../architecture/observer-compatibility.md)。

目录格式、缓存预算、查询与来源撤销、schema 1→2 兼容、按需正文和连续阅读见[历史首页详细设计](../design/history-home.md)。缓存覆盖与源正文可用范围分别表达，保留 partial 和省略原因。

明确启动、显式恢复与丢失启动响应后的只查询恢复不根据最近模型 metadata 自动接管 CLI；恢复所选历史前须停止同项目的现有 Run。最多 4 个活动 Run、4 个已结束内存阅读；监听与地址发现共享，授权、撤销和停止仍按 Run。设备配对不获得全局来源或启动管理权限；有效 RemoteOwner 的权限由中转契约另行限定。

展示项目归属与 recordedCwd 恢复校验分离，整理操作只更新独立归类状态和派生缓存，不改原始历史。非空 `history_base` 的继承历史可读但暂不恢复。规模、旧库和手机支持限于[支持文档](../guides/support.md)列出的范围，不从架构采用推导任意负载性能或全库完整性。
