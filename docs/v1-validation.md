# V1 验证记录

Date: 2026-08-14  
Observer version: 0.1.0  
Observer schema: 11<br>
Codex source baseline: `41ece455b7fa7166f4fc38522952afdaa2604e18`  
Codex CLI used for live compatibility: 0.146.1  
App Server v2 schema SHA-256: `1a193fc005458d9a06642adf81350fc6280f64f50558ab34cdd0b75e20d164d9`

## Automated gates

```text
cargo test --all-targets                         60 passed, 1 explicit capacity test ignored
cargo clippy --all-targets -- -D warnings       passed
cargo build --release                            passed
node --check web/app.js                          passed
jq empty compatibility/*.json fixtures/*.json  passed
git diff --check                                 passed
```

覆盖的关键回归包括：

- plain/zstd、半行、坏行、oversize、archive rename、representation sibling、content fingerprint；
- legacy 与 paginated rollout、unknown 顶层类型、parent/fork/sub-agent/history base；
- raw/projection/checkpoint 事务、migration rollback/retry、retention/tombstone/blob rebuild；
- Thread/Turn/Item/Search 签名 cursor、筛选绑定、retention 410、snapshot + event replay 水位；
- 两个 `CODEX_HOME` 中相同 Codex Thread ID 的复合身份隔离；
- token/Origin、secret redaction、symlink/no-follow、blob Range、只读 pending request；
- export 不覆盖且只输出脱敏数据；purge 审计、抑制墓碑与强制重放不复活。
- schema 10 私有文件 mode 自动收紧、redaction v2 与多 Turn completeness/clean EOF 聚合；
- schema 11 单 DbWriter、有界队列/CommittedEventBus、epoch continuity、pending resolved 与可重建 projection conflict；
- 一次性配对/重放/过期/token 轮换、运行中只读 snapshot export、capture policy 与独立 delta retention。
- `SQLITE_FULL` / `SQLITE_IOERR` commit 前 failpoint 会完整 rollback 且不发布，100 consumer fan-out 相互隔离；丢失 watcher hint 后周期 rescan 可恢复；统一错误体包含 requestId/retryable 且不含 payload/path。

## Release binary E2E

使用 `fixtures/observer.fixture.toml` 启动 release binary，完成以下检查：

- `doctor --json`：schema 迁移后 healthy；对全新缺失配置路径仅报告 degraded，未创建目录、数据库、lock 或 blob；
- REST：health、sources、Thread cursor、Search、unknown/raw event、capabilities 可查询；
- Viewer：`/` 与 `/app.js` 返回正确 content type；长列表由 Viewer 自动消费 cursor/event sequence；
- Security：无 token 为 401，非法 Origin 为 403，存在 CSP、`no-store`、nosniff、DENY frame 与 no-referrer；
- SSE：`Last-Event-ID: 16` 配合 method filter 只恢复 event 17；
- WebSocket：完成 101、subscribe ack，并从 cursor 16 按 method filter 回放 event 17；
- Export：文件 mode `0600`，JSON 可解析，未出现 fixture secret；
- Purge confirmation：缺少 `--yes` 时退出 1，目标 Thread 仍存在。
- Pairing/Viewer：兑换 200、重放 401、非法 Origin 403、Cookie 认证 SSE 200、Viewer 200；404/405 均返回统一错误契约；
- Runtime export：daemon 持有 writer lock 时，独立只读 WAL snapshot export 成功且输出 mode 为 `0600`；
- Schema/source health：health 报告 schema 11，`/v1/sources` 返回 event/gap/decode/unknown/connection continuity 字段，`/v1/meta/settings` 只返回脱敏后的生效配置。

既有官方 App Server fixture probe 覆盖 `observe_new`、`attach_loaded`、第二连接、pending request 零响应、resume 局部失败、unsubscribe 与 close handshake。本轮确认官方源码仍位于 commit `41ece455`，但验证环境只有 Codex IPC socket、没有配置的 app-server control socket，因此周期 archived/non-archived list/read 对账的真实双连接门槛未重跑；发布 Live preview 前仍需补跑。Live 默认关闭并标记 opt-in preview。

## Capacity smoke

自动化用例在一个事务中构造 10,000 Thread 和单 Thread 10,000 Item/FTS 记录，然后验证 Thread 首页与 Search 首页均返回 50 条及下一页 cursor。首次审计发现 Search JOIN 为近似二次扫描，单用例耗时 29.59 秒；schema 8 增加 `(thread_key, item_id)` lookup index 后，同一用例总耗时 0.22 秒（包含数据构造与两次查询，开发构建）。schema 9 另包含已有数据库的 unknown rollout 状态回填，保证升级与新导入结果一致。

详细设计中的 10 GiB / 2,000,000 raw event / 100 并发客户端属于 SLA 冻结前的原型基准数据集；V1 未对未实测规模承诺 SLA。仓库提供忽略的 `two_million_event_capacity_path` 显式测试，需以 `OBSERVER_RUN_CAPACITY=1 cargo test two_million_event_capacity_path -- --ignored` 单独运行；常规 CI 不执行。当前验证证明查询有正确索引和有界分页，不替代目标机器上的完整容量规划。

## 明确限制

- Store-first durable history 是 V1 正确性主链路；App Server live attach 不是默认能力。
- HookAdapter 是可选 wake-up 优化，未实现时 watcher + 周期 rescan 仍保证最终发现。
- 不承诺 macOS App 私有启动拓扑，也不依赖 Codex 私有 SQLite schema。
- 不实现 V2/V3 mutation、approval response、turn send、interrupt 或 IM bridge。
