# R6-A：独立旧历史 reader 与零写验证

日期：2026-09-22。代码基线 `a1f96e5`，分支 `codex/native-cli-live-workbench`；本文记录其后的本地未提交增量。实施状态只在[唯一计划](../v2-implementation-plan.md#r6-history-home)维护。

## 1. 环境与范围

- 当前 macOS / Apple Silicon；Rust 1.95.0，rusqlite 0.37，bundled SQLite 3.50.2。未将结果扩展到 Linux、Windows 或其它 SQLite/VFS 版本。
- 全部数据由临时目录内的 schema 1–20 SQL 与合成记录生成。25 fixture 只增加先前观察到的七张表和 `image_uploads.queue_entry_id`；新增表是结构哨兵，不宣称复原历史 migration 21–25。
- 本次没有读取或导出用户会话正文，没有打开真实旧库的新 reader，没有修改原生历史、旧 Observer 数据或正在运行的用户实例。先前本机 schema 元数据对照仍是结构证据；真实产品读取抽查归 R6-G。
- 此增量未接 Web 路由或默认首页，未新增 CLI 参数、JSON 配置版本或数据库 migration。现有无参数启动仍进入单项目原生工作台。

## 2. 实现与契约

实现位于 [history 模块](../../src/history/mod.rs)、[reader](../../src/history/legacy_reader.rs)、[能力清单](../../src/history/legacy_contract.rs)、[文件读取](../../src/history/files.rs)和[只读 VFS](../../src/history/readonly_vfs.rs)。只复用 `domain/redact.rs` 的纯 JSON 脱敏，不依赖旧 Database/Importer/Writer/HTTP 启动。架构测试限制旧控制、migration 和写入模块回流。

`LegacyReader::open(sourceId, dbPath, blobDir, budget)` 返回按集合列出的能力；`page(Query, cursor, limit, budget)` 返回 sourceId、sourceRevision、records、nextCursor 和 issues。支持项目、会话、上下文、父子/fork 关系、轮次、条目、来源、epoch、缺口和受限 raw。只按固定表/列构造 SELECT，范围值参数绑定；视图、虚表、非约定列类型和被覆盖的 rowid 不作为历史表使用。缺少上下文列只关闭对应能力；未知版本标记 `unvalidated_schema`，保留已有契约可读部分。未知 JSON 字段/状态保持来源事实，不变成成功或完整。

- 内部 adapter 按 rowid 做稳定 keyset 分页，每页 1–100 条；项目按分组最小 rowid 分页。用户可见活动时间排序归后续目录索引。
- 单字段 256 KiB，页记录序列化预算 2 MiB，超大字段置空并列出原因；坏 JSON/标量类型分别标记，不回显原内容到错误。
- 游标带实例密钥校验，绑定查询范围/集合与来源修订。主文件替换、schema/user_version 改变或页间源变化要求重新读取，不能拼接不同修订。
- 80 ms busy timeout、最多 2 秒查询预算、SQLite progress handler 与可取消标记；页内短事务结束即释放。取消/锁超时后 reader 仍可复用。调用方须放在后台 worker，不直接在 HTTP executor 扫描。
- `blob(threadKey, eventSeq, sourceRevision, budget)` 通过同会话的 raw event 解析附件，不能用客户端路径或任意 blob ID 读取。仅支持旧格式 JSON，最大 4 MiB，核对大小及 BLAKE3 哈希并再次脱敏；缺少附件目录只关闭附件能力。
- 数据库/辅助文件和附件要求当前用户所有；拒绝非普通文件、硬链接、主文件叶子符号链接。附件使用固定根 fd、逐段 `openat`/`O_NOFOLLOW`/`O_NONBLOCK`，检查目录替换、文件身份和内容变化。

## 3. 零写方式和边界

SQLite 的主库只读标志不足以约束共享内存文件；官方也区分 WAL/SHM 是否存在及其可读写条件，见 [SQLite WAL 只读说明](https://www.sqlite.org/wal.html#read_only_databases)。实现针对当前 bundled 源码验证 `readonly_shm=1`，并使用不替换默认 VFS 的专用只读 shim：

1. 主库、WAL、rollback journal 的打开都强制 READONLY/NOFOLLOW，清除 CREATE/READWRITE/DELETEONCLOSE。
2. 拒绝写入、截断、删除与会改写文件的 file control；不提供 mmap 写通道。`query_only`、defensive/trusted-schema/view/trigger 限制另作连接保护，不能作为 VFS 零写的替代。
3. 保留原 VFS 锁和 SQLite 的 WAL 一致性协议。SHM 不扩展、不删除；如果同进程已有写连接导致原生 VFS 复用可写映射，直接拒绝读取，不交给 SQLite 使用。
4. 不使用 `immutable`/`nolock`、整库复制、修改源 journal mode、checkpoint、恢复热日志、补建辅助文件或 migration。

已验证 DELETE 模式普通库、另一进程持有的活动 WAL、写者未提交事务、WAL 写者异常退出后的现有 WAL/SHM。每个冻结检查窗口内，对所有源文件比较内容、inode/size/mtime/ctime；写者主动提交之前/之后分别取基准，不能把写者的合法变化归为 reader 写入。外部已提交内容可见，未提交内容不可见，页间提交使旧 cursor 失效。

**限制：** 需要新建 WAL/SHM、修复热日志或重建不可安全读取的辅助状态时，来源返回不可用，原文件不修复。当前同进程 writable SHM 也拒绝；生产装配不得在新应用进程内启动旧写内核。仅有 WAL 模式主文件不等于已经具备此 reader 的读取条件，后续真实来源接入必须保留此降级，不能默默切换普通读写连接。当前测量是本机文件，不承诺网络文件系统行为。

## 4. 已执行验证

[合成测试](../../src/history/tests.rs)覆盖：

| 验证组 | 结果 |
| --- | --- |
| schema 20/25 全集合读取、额外控制/审计哨兵 | 输出一致；所有源文件内容及写入元数据不变 |
| 未知版本、缺列、未知列、坏 JSON、错误标量、视图/伪 rowid | 结构化降级；不伪造成功，不回显坏 payload |
| 多页、过滤、篡改/跨 reader cursor、写者提交、schema/文件替换 | 旧游标/reader 被拒绝，未出现重复拼页或跨范围返回 |
| 活动 WAL、长写事务、写者退出、缺 WAL/SHM、热 journal | 可读场景一致；不可读场景明确报错；不补建/恢复/删除源文件 |
| 同进程可写 SHM、禁用 query_only 后尝试 DML/DDL/version/VACUUM/extension | 仍拒绝写入；源文件不变 |
| 5 万会话查询的超时与执行中取消、锁等待 | 有界停止；停止后可重新查询 |
| 附件正文、scope、大小/hash、父目录/叶子 symlink、硬链接、路径穿越 | 合法附件读取脱敏；越界/不一致拒绝 |
| V1 对照 | 同一合成 schema 20 的会话、turn、item 公共字段与 V1 API 相同；旧只读/退役路由断言继续通过 |

V1 对照位于[既有兼容测试](../../src/http/legacy_history_tests.rs)，不把旧 HTTP/Writer 引入产品 reader。子进程 writer harness 默认标记 ignored，仅由父用例显式启动并回收；不是把 WAL 验收留作未运行。

最终 `cargo test --offline --all-targets -- --test-threads=4`：**359 通过、38 默认忽略、0 失败**（库 259、Observer 94、退役集成 1、examples 5）。其中新增 reader 相关 18 项通过，包含纯脱敏和 schema 资产回归。未重跑未改动的前端/Chrome/真实 CLI 环境矩阵；本片没有 UI 或原生参数变化。

`cargo fmt --all --check`、`cargo clippy --offline --all-targets -- -D warnings`、`git diff --check` 与 `cargo build --offline --bins` 均通过；Markdown 检查覆盖 11 个修改/新增文档、346 个本地路径/锚点及表格/围栏，全部通过。没有 migration、配置格式变更、提交、push 或发布。

## 5. 回归中发现并修复的设置启动等待

首次全量高并发出现 7 项既有设置/设备/管理超时；改为两线程仍有 2 项，串行仍有 3 项，单独执行则通过。因此不能仅归因于测试并发，也没有放宽超时或跳过断言。

检查发现 ConfigService 在消费请求前同步初始化 OS watcher；其 Reload/周期读取结果没有消费者，也没有改变任何运行缓存。设置每次查询/保存本来就直接读盘，管理器本来就每秒检查策略并在每次删除前再次检查。移除这个冗余 watcher 后保留上述实际读取路径及有界工作队列，最终完整回归通过。旧通知合并 helper 测试替换为重复建立 12 个服务、立即读外部编辑/损坏配置的行为回归；原外部编辑、revision 冲突、自动清理、管理 API、设备启用测试保持原断言。

对应 [config 实现](../../src/workbench/config.rs)、[配置行为测试](../../src/workbench/config/tests.rs)及[配置契约](../codex-native-cli-workbench-history-settings.md)。这是进入 Application 生命周期拆分前消除已有初始化等待，不新增设置项，也不改变原生 CLI 配置。

## 6. 复现命令与下一步

```sh
cargo test --offline --lib 'history::tests::'
cargo test --offline --bin codex-observerd schema20_copy_v1_queries_export_and_rejected_mutations_preserve_history_and_audit
cargo test --offline --all-targets -- --test-threads=4
cargo fmt --all --check
cargo clippy --offline --all-targets -- -D warnings
git diff --check
```

下一步为 R6-B：独立 Application/Run、零 CLI 首页壳、应用配对与 entry v2、重复启动复用。R6-A 是底层契约通过，不是完整历史首页或 R6 用户试用交付；全局来源配置、索引、UI、启动/恢复、多项目与手机隔离仍待各片实现。
