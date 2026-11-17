# Codex 历史存放与项目映射

## 范围与证据基线

本研究解释原生历史 reader 能从哪些字段取得身份和目录，以及为何不能仅凭 rollout 重建官方项目归属。不涉及修改官方 CLI、读取其私有 SQLite 表、自动启动 app-server、恢复或迁移用户记录，也不规定新的 Git/worktree 聚合能力。

2026-11-17 只读核对官方仓库，实际 commit 为 `c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee`。下列源码链接全部固定到该 commit；协议、实现和测试代码已交叉阅读，未执行官方测试。该源码版本不等同于当前安装的 CLI，也不能证明更早版本或 Desktop 提供相同实验能力。现场字段覆盖与本项目验证结果保存在受管产物，不提交私人记录作为 fixture。

## 文件布局负责定位，不负责项目归属

| 来源 | 当前源码实现 | 对 reader 的含义 |
| --- | --- | --- |
| `sessions/YYYY/MM/DD/` | [路径生成](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/recorder.rs#L1684)按创建时本地日期生成；SessionMeta.timestamp 另转为 UTC | 日期目录不是项目目录，不能由它推导项目 |
| `archived_sessions/` | [目录常量](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/lib.rs)区分活动与归档历史 | 归档位置不改变项目身份 |
| `rollout-<timestamp>-<thread-id>.jsonl` | [文件名解析](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/rollout_file_name.rs)还支持 revert 后 `<stable-thread-id>_<rollout-id>` | 文件身份、稳定 thread 与 immutable rollout 不总相同；文件名不携带项目 |
| `session_index.jsonl` | [名称索引](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/session_index.rs)包含 id、thread_name、updated_at | 可用于名称查找，本身没有目录或项目映射 |

## 官方项目与执行目录是独立模型

[Project 协议](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server-protocol/src/protocol/v2/project.rs#L15)包含 opaque ID、name、零到多个 roots 及排序/时间信息。[Thread](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs#L237)分别表达 `projectId` 和 `cwd`，前者是 app-server 维护的 canonical assignment。项目 ID 不是路径的哈希，项目也不保证恰好有一个目录。

当前源码已提供 `project/list`、`project/read` 等[实验 API](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server-protocol/src/protocol/common.rs#L820)。线程可以显式指定或[更新归属](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server-protocol/src/protocol/v2/thread.rs#L1066)，项目与 cwd 的列表筛选也相互独立。创建项目不会自动扫描 cwd 收纳全部线程；导入需明确 thread ID。

本机实现把项目和 thread assignment 存在 state 数据库中。[实现注释](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/thread-store/src/local/update_thread_metadata.rs#L760)明确指出 project preferences 仅存在于 state database；[迁移结构](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/state/migrations/0049_projects.sql)及[持久化/重启测试](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server/tests/suite/v2/projects.rs#L210)与此一致。完整 [SessionMeta/SessionMetaLine](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/protocol/src/protocol.rs#L3505)没有 `project_id`。仅扫描 JSONL 无法重建数据库独有的当前项目意图；不能据此断言用户的数据库里也没有映射。

旧 server 允许[省略 projectId](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs#L300)。缺失既可能是版本/能力限制，也可能是未归属，不能统一解释为用户主动选择了“无项目”。

## 可解析字段及其边界

| 字段 | 官方当前语义 | 本项目采用的边界 |
| --- | --- | --- |
| `SessionMeta.id` | 当前 thread ID；[reader](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/recorder.rs#L1171)以首个 SessionMeta 作为 rollout 自身身份，后续可能复制 fork 历史 | 保留自身可靠身份。仅当后续异 ID 正好等于自身首次可靠身份声明的有效 `forked_from_id` 时，作为继承 context 保留，不覆盖项目、cwd、类型或父关系；未声明异 ID 仍冲突 |
| `SessionMeta.cwd` | 创建时捕获的工作目录 | 保留为 recordedCwd；目录分组标 `cwd_inferred`，不声称等于官方项目 |
| `runtime_workspace_roots` | 创建时顶层运行工作区 roots；absent 为 unknown，空数组为无 roots，允许多个 | 工作区/能力信息，不选取首个 root 充当项目，不据包含关系合并目录 |
| `TurnContext.cwd` | turn 的执行上下文；[state 投影](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/state/src/extract.rs#L129)仅在 session cwd 为空时补值，[回归](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/state/src/extract.rs#L529)禁止覆盖已有 session cwd | 不把任意最近 turn 的目录当作用户项目，尤其不混入 fork 继承的父 turn |
| `ThreadSettingsApplied` | [state 投影](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/state/src/extract.rs#L169)会更新该线程的运行设置和 cwd | 不在本次归属修补中改变 recordedCwd/恢复校验；若后续适配须另验证事件归属与继承边界 |
| parent、fork、session、attachment、section | 代理父关系、历史 lineage、session tree、附着和用户分类分别有独立字段 | 均不能替代当前 projectId；创建时 fork 继承项目不证明之后仍同项目 |
| `git` | 捕获的 commit、branch、repository_url，均可缺失；[记录器](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/recorder.rs#L1951)未持久化本机 repo root | 同远端、同分支、同 basename 均不足以证明同一项目 |

官方恢复候选查找还会[参考最近 TurnContext.cwd](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/rollout/src/recorder.rs#L2165)，它与 state 的 cwd 投影服务不同用途，不构成统一项目映射规范。

## 仓库范围检索与项目归属不同

官方 TUI 有显式 Repo filter，通过[Git 管理目录与双向 worktree 链接](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/git-utils/src/worktree.rs#L14)识别 linked worktree。[过滤实现](https://github.com/openai/codex/blob/c0cc9fc820f62e4dbdde68214dd6d792d6c8a7ee/codex-rs/tui/src/resume_picker.rs#L2136)要求 common_dir 和 relative_cwd 都相同，再转为多个 cwd 精确查询；不是把相同 remote 的全部 clone 或同仓库所有子目录归为一个项目。

它依赖当前仍存在的 Git 元数据，目录消失或迁移后不能回推历史意图。本项目继续遵守已确认的“不做 Git/worktree 自动聚合”，没有因此扩大文件访问或扫描整台电脑。

## 本项目修补与后续接口条件

已实施的修补是识别明确 fork 继承的 SessionMeta，保留自身 thread/cwd/项目和辅助类别；按需阅读窗口使用已发布、绑定来源修订的 catalog 元数据，不能从窗口中途的父元数据重新推断归属。自身可靠身份建立后，缺失/无效/nil ID 的后续 metadata 不覆盖归属；同 ID 却修改 cwd、来源或其他已采用身份字段也只保留 context、记录冲突并禁用恢复。相同 metadata 重复不会清除已有冲突。首次 metadata 身份不可靠时继续可读，但后续可靠声明不能使它获得恢复资格。派生 catalog 升至 6，重读不变来源，独立人工归类状态保留。恢复仍经过原有首行身份/cwd、来源修订、唯一文件、history_base 与目录校验，不启动 CLI 来验证分类。

详情和归属编辑显示现有依据：工作目录推断、工作台明确目录、来源记录或用户调整。该说明不声称已取得官方项目关系；人工调整仍只影响当前来源记录的展示。

若以后接入官方项目，需要先验证实际 server 的公开实验能力，独立保存来源/server 能力、native thread ID、opaque native project ID、名称与全部 roots，并区分本项目路径型项目 ID。不能直接绑定私有表、把 native ID 当路径，或把多 root 项目压成一个 cwd。`thread/list` 的普通读取可能修补索引；即使使用 `useStateDbOnly`，也不能由请求名推导 app-server 启动和整个来源均为零写。当前尚未实现该适配，也未验证用户环境的实验接口或数据库覆盖。直接声明之外的更深 fork 继承链仍保守降级，不能从受测的双 metadata 形状宣称普遍兼容。
