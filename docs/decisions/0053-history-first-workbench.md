# ADR 0053：历史首页、按项目启动与旧历史只读整合

- Status：Accepted（已确认的入口、只读整合、显式启动/恢复与多 Run 决策已实现，并有 A–F.1 分片验证；规模/隔离与限定旧库抽查已有 G1–G3 证据，完整 R6 交付和用户试用仍以实施计划为准）
- Date：2026-09-22

## Context

R0–R5 将普通官方 CLI、PTY、模型代理和工作台直接绑定为一次启动。旧 V1 跨项目历史保留在独立 `codex-observerd`，新 `codex-view` 页面没有入口，且当前迁移上限 20 无法直接启动使用 schema 25 的实际旧库。只保留代码和 schema 20 fixture 没有完成用户的历史阅读闭环。

用户要求将这些能力整合为 R6，倾向默认先看到全部历史，然后进入已有项目或指定新路径启动 CLI；并明确确认：另一个项目在新工作台标签页启动，保留原项目；项目入口默认新对话，历史详情另有“继续此会话”。

只读表结构对照发现，实际 schema 25 保留 schema 20 的核心历史表列，新增内容主要是旧控制队列/上传表；这支持独立 reader 方案，但不证明正文语义与 WAL 兼容。源码、实际结构核查与证据限制详见 [R6 设计](../codex-native-cli-workbench-history-home.md#2-已核对的事实与证据限制)。

## Decision

采用以下决定；代码已实现的行为与证据边界见 Consequences，执行门槛以[实施计划 R6](../v2-implementation-plan.md#r6-history-home)为准：

1. `codex-view` 默认启动 Application 与全部历史首页，零 CLI/PTY/模型代理；CLI 安装/profile 校验延后到明确开始工作。保留显式目录/会话直达入口，不自动恢复最近任务。
2. Application 拥有同源网页、配对、配置和历史目录；独立 WorkbenchRuntime 拥有一个项目的原生 CLI/PTY/Proxy/Recorder。内存 RunRegistry 隔离并行项目，同项目重复点击进入已有 Run。它不是持久控制账本，不恢复旧 Worker/App Server。
3. 历史读取由工作台记录、原生 rollout、旧 Observer SQLite 三类 adapter 提供；后台有界索引/脱敏搜索与按需正文读取独立于实时热路径。前端统一项目目录与阅读样式，保留每条来源和能力，不把多个来源拼成虚假的完整会话。
4. 旧库原位只读，初始验证 schema 20 与实际 schema 25 的读契约。禁止 migrate/import/rebuild/purge、修改 user_version、恢复旧控制表行为、整库复制或对活动 WAL 使用 immutable。只读 SQLite/VFS 与辅助文件零写的可行性先用合成数据验证。
5. 全局缓存和配置保存在 `.codex-web` 的既有目录体系，索引可重建。新 Run 补充项目路径 sidecar；旧 Run 无路径时不猜。新增来源登记和设置仍是当前页展开面板，不建独立设置界面。
6. 目录启动是本机所有者明确操作，目标路径/身份/配置修订需重验。会话恢复使用可信 SessionMeta.id 和对应 nativeHome；点击历史只读，不向 CLI 重放正文/Enter。
7. 跨项目可读不扩大原清理范围或已有手机授权。手机配对绑定 Run，新旧路由均校验目标；其它项目历史、全局来源、启动管理对现有手机授权不可见。模型代理仍仅 loopback。
8. 默认前台运行；同配置/数据根的重复启动通过私有实例发现复用应用，不加 OS daemon。退出仅回收本应用的 Run；首页或其它 Run 不因某一项目停止而退出。

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

先完成 R6-A 的只读兼容探针，再逐片接入首页、目录、阅读与 Run。列一致、静态原型、默认忽略的测试或当前 R5 Passed 都不能替代 R6 实际验收。R6-A 已以专用只读 VFS + readonly_shm 的方式验证不写源文件，保留 SQLite 锁和一致性协议；需要补建/恢复辅助文件的情况返回不可用，不用 immutable 绕过。证据和范围见[R6-A 记录](../validation/native-cli-r6-a-legacy-reader-2026-09-22.md)。B 已接入 Application 与单 Run 装配及首页，C/D 已接入目录与阅读；不运行或替换用户服务。

R6-B 验证见[独立应用与首页](../validation/native-cli-r6-b-application-2026-09-22.md)。该阶段将默认入口切为无 CLI 首页；跨项目目录/来源登记已由 C/D 接入，E/F 的项目启动及并行运行现已实现；分片结果不代替整片验收。

R6-C 的具体目录格式、缓存预算、查询/来源撤销及 schema 1→2 兼容实现见[R6 §10](../codex-native-cli-workbench-history-home.md#10-r6-c-目录与来源的实现契约)。它保留独立正文与 partial 信息。R6-D 已接入项目/来源/会话导航、三类按需正文和详情、有界连续阅读及位置恢复，具体契约见 [R6 §11](../codex-native-cli-workbench-history-home.md#11-r6-d-阅读界面与按需正文)，证据见 [R6-D 验证](../validation/native-cli-r6-d-reading-2026-09-22.md)。R6-E 已实现网页目录启动、显式恢复、丢失启动响应后的只查询恢复，以及单运行项目停止后的新 Run；它不根据最后观察到的模型 metadata 自动接管现有 CLI，恢复所选历史前须停止同项目的现有 Run。该实现已完成本片验证，报告[验证记录](../validation/native-cli-r6-e-launch-2026-09-22.md)。F 已扩展最多 4 个活动 Run、4 个已结束内存阅读，取消 Application 的无 Run 前缀别名；共享设备监听与地址发现，授权、撤销、停止仍按 Run。settings 因共用配置限电脑 owner，手机不获得全局来源路径或启动权限。见 [F 实现契约](../codex-native-cli-workbench-history-home.md#r6-f-multi-project)与[分片验证](../validation/native-cli-r6-f-multi-project-2026-09-23.md)。F.1 已将展示项目归属与 recordedCwd 恢复校验分离，并重建派生缓存，见[纠偏记录](../validation/native-cli-r6-f1-project-classification-2026-09-23.md)。

G1 已有[规模与故障](../validation/native-cli-r6-g-scale-2026-09-24.md)及[资源补充](../validation/native-cli-r6-g1-resources-2026-09-30.md)证据；G2 的[历史压力与运行隔离](../validation/native-cli-r6-g2-isolation-2026-09-25.md)和 G3 的[限定旧库抽查及兼容回归](../validation/native-cli-r6-g3-compatibility-2026-09-29.md)分别说明受测范围，不承诺任意负载性能或全库完整性。非空 history_base 的继承历史仍可读但暂不恢复，真手机多项目体验不据合成浏览器结果扩展。Accepted 表示采用且实现这些架构决定，不表示整片 R6 验收已通过；当前状态与剩余用户试用只在实施计划维护。
