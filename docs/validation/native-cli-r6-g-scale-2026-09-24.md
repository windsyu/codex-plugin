# R6-G：历史规模与故障验证（2026-09-24）

本轮在用户确认 A–F.1 试用通过后开始。已验收内容形成本地提交 `466dff7 feat: add history-first workbench with isolated project runs`，未推送。本记录只作为 G 的证据；实施状态和剩余任务统一在 [实施计划](../v2-implementation-plan.md#r6-g规模故障和用户试用交付) 维护。

## 1. 环境、语料与测量边界

- Apple M3 Max / 36 GiB 内存，macOS 26.5.2，本机 debug 构建；系统已安装 Chrome，无额外浏览器下载。
- 每轮使用临时 HOME/USERPROFILE/CODEX_HOME、临时来源及派生目录；不写用户原生历史，不替换用户运行的实例。
- 全量语料为 10 个项目、10,000 个会话、200,000 条合法 JSONL 消息，实际写入 2,306,150,000 字节（约 2.15 GiB），不是稀疏文件或超长坏行。每条有唯一可读文本标记和 11 KiB 的合成图片 data URI；图片沿已有规则省略，文本可读。另用大段可读正文测试缓存压力，不能将图片省略的比例解释为任意正文压缩率。
- 保持默认 `cacheLimitMiB=512`，不通过放大配置消除预算压力。另有 64 MiB、160 会话的小型常规回归。
- “冷”指新临时目录、没有派生索引；源文件刚生成，未驱逐操作系统页缓存，不能解释为冷磁盘 I/O。浏览器发布时间从页面导航开始，索引在服务启动时已开始；不能与库启动计时混列。DOM/堆采样从首屏交互后开始，不覆盖最早启动阶段。目录 API、完整发布、浏览器可交互分别计时。元数据查询每类 30 次；CPU/RSS 的 `getrusage` 值属于整个测试进程，包含语料生成，不能解释为独立索引线程的消耗。浏览器用 CDP 及每 500ms 一次的进程采样，不声称获得瞬时峰值；`ps %cpu` 是进程历史平均值的采样最大值，不是 500ms 区间 CPU。

## 2. 发现的问题与修复

### 2.1 正文缓存挤占目录

修复前，在默认预算下全量语料仅发布 2,368/10,000 个会话。来源虽然提示 `cache_budget`，其余会话却无法从目录进入。小型回归稳定复现 159/160，刷新、重启仍然缺少同一条记录。

原先正文与元数据共享每来源材料额度，正文只留出固定 64 KiB 的余量，随后目录元数据超限会结束扫描。现在为后续目录保留至少半份来源材料额度；正文/搜索先停止缓存，继续收录元数据和源定位信息。元数据自身也受总预算约束，不能无限增长。

旧检查点复用执行相同策略：可在事务内省略旧正文和搜索，保留元数据、cwd、定位符和检查点；若元数据本身超预算则回滚。正文覆盖及来源 partial 标记继续保留，不能在重扫后变成完整。原始记录可通过已有按需窗口读取。

不新增配置，不迁移原生、Observer 或 journal 数据；派生格式仍为 2。下一轮扫描即可应用新预算规则。材料预算与 SQLite 实际文件大小不是同一指标；现有数据库页硬限和事务空间预算继续生效。

### 2.2 大段文本脱敏开销

普通文字此前逐字符进入状态机。新增安全起始字节表：仅当没有待判定前缀、没有正在遮罩的值、且不是工具字段模式时，成段复制不可能开始凭证的文字；遇到候选字节立即回到原有规则。

测试保留原 `push` 作为参考，逐块比较输出、pending、mask 和 finish，覆盖 Unicode、单字符密钥、32 个密钥、8192 字节密钥、重叠前缀、工具字段和任意分块。参考仍共享未改动的 drain/finish，因此这次差分验证不等于今后整个算法永久冻结。独立 GPT-6 审查未发现本次优化的阻断问题。

同一 debug 微基准（1 MiB ×3）：中文原实现 680.187ms，新实现 9.640ms；英文 2,124.873ms → 1,703.229ms。这是特定纯文本函数的测量，不能宣称整个历史索引提速 70 倍。

## 3. 规模结果

| 测量 | 修复前 | 修复后 |
| --- | --- | --- |
| 64 MiB 小型回归目录 | 159/160 | 初次、刷新、重启均 160/160，省略正文的源窗口可读 |
| 100 会话 smoke | 100/100 | 100/100，无覆盖缺口，不变重扫 revision 保持 |
| 全量目录 | 2,368/10,000 | 10,000/10,000，ID 全集对账，无遗漏、重复或额外条目 |
| 全量首次发布 | 40.795s，但目录被截断 | 103.820s，完整目录发布 |
| 全量冷目录 API | 1.587ms | 0.341ms；不是浏览器交互时间 |
| 全量发布后派生目录 | 88,399,872B | 69,427,200B |

正文缓存保留 24,255 条记录、16,072,980 字节（记录含来源上下文）；源语料有 200,000 条 item，两者不能混称。搜索首标记命中 1、尾标记命中 0，均明确 partial，不能宣称全文覆盖。

最终完整复验（`/tmp/r6-g1-full-default-fixed-v2.log`）每类 30 次、均无查询错误：

| 暖查询 | p50 | p95 | 最大值 |
| --- | --- | --- | --- |
| 会话列表 | 24.40ms | 36.71ms | 50.30ms |
| 项目列表 | 69.33ms | 93.36ms | 115.93ms |
| 项目筛选 | 21.22ms | 47.03ms | 47.64ms |
| 后段分页（200条） | 83.45ms | 124.95ms | 127.34ms |
| 搜索命中 | 135.46ms | 151.09ms | 151.43ms |
| 搜索未命中 | 145.54ms | 162.49ms | 163.85ms |

另一个默认预算压力样本使用 160 会话、3,200 条较长英文消息和图片，实际源 91,496,800B；160 条目录均保留，正文/搜索显式 `cache_budget`，派生 34,369,536B，总耗时 113.39s、进程 CPU 113.30s、峰值 RSS 27,885,568B。此项验证预算降级行为，不设吞吐通过门槛；它也说明大段可读英文仍有明显解析/脱敏成本。

四类元数据 p95 均满足 200ms 目标。取消请求到下一查询恢复 162.54ms；此值不能证明活动 SQL 在任务 abort 时已终止。未变化重扫 9.745s，已确认 generation 更新且 revision 保持；重扫后派生目录 138,956,800B。暖重启首次非空列表 33ms。整个测试进程 CPU 173.67s，其中生成阶段约 54.42s；生命期峰值 RSS 47,988,736B。

首轮修复后测试的重扫探针曾把 SQLite 短暂 busy 导致的 `None` 误当新 generation，且未断言重扫通过。已修为必须取得不同的有效 generation，并断言 ready/revision/warm 非空；重新跑完整同规模语料取得上述最终数据。该首轮的查询数据有效，重扫结果未计通过。

### 3.1 正式二进制与系统 Chrome

全量浏览器检查通过（`/tmp/r6-g1-chrome-full.log`，Chrome 153.0.8010.53，1280×900）：

| 测量 | 结果 |
| --- | --- |
| 导航至展开设置可操作 | 89.60ms（目标 ≤1s） |
| TTFB / FCP | 6.5ms / 56ms |
| 导航后观察到完整发布 | 103.133s，10,000条、partial |
| 首次自动显示会话列表 | 103.133s，无需手动点击更新 |
| 暖列表 HTTP（30次） | p50 20.9ms / p95 46.3ms / max 53.5ms |
| 会话列表挂载 | 30条 |
| 采样堆 / DOM 最大值 | 5,503,352B / 1,810节点 |
| 服务 RSS 采样最大值 | 41,440 KiB |
| 进程历史平均 CPU 百分比的采样最大值 | 95.4% |
| 采样次数 | 209次，间隔500ms |
| 页面异常 / 外部请求 / 终端 WS / CLI 调用 | 均0 |
| 测试服务退出 | 51.59ms，入口文件已移除 |

每次暖查询断言30条记录、有效revision和entryId后才纳入耗时；p95不包括React列表渲染。随后手动更新并进入会话，必须出现真实合成正文标记且无阅读错误，不能仅以阅读面板外壳可见判定成功。测试结束派生目录72,785,920B；目录写入期间的测量不等于两代重扫完成后的大小。

库级全量结果与浏览器结果起点、进程和采样范围不同，不相加或互相替代。最后对库级脚本追加“冷API必须成功、取消后查询必须恢复、刷新前必须取得有效旧generation”三个断言，并通过100会话smoke复核；没有在追加这三个断言后再次运行10k库级脚本，已有全量日志中的实际响应及恢复结果均成功。全量浏览器使用上述加强后的最终断言运行。

## 4. 故障与并行负载

- `sqlite_full_rolls_back_document_without_publishing_staging_or_changing_source`：在临时 SQLite 中按现有页数限制 `max_page_count`，经 SQLite 错误码确认 `SQLITE_FULL`。生产写入返回 `cache_write_failed`，事务和五张关联表回滚，先前暂存记录不公开，已有发布代、列表、正文、源字节与签名不变。使用约 136 KiB 数据触发，不填满真实磁盘。此项覆盖生产写入/查询函数，不冒称整个后台周期故障注入。
- 已有测试继续覆盖损坏派生索引重建、未发布代丢弃、源更新/替换/删除/归档、链接拒绝和撤销来源。
- `history_library_pressure_keeps_sse_pty_and_durable_recording_live`：128 会话的后台解析、SQLite 独占锁和 64 个并发查询下，本地 SSE 字节转发、PTY 回显、Recorder 持久水位继续推进。一次专项：解析期首块 111.91ms、PTY 16.69ms、持久化 242.15ms；锁期分别 112.39ms、19.45ms、246.11ms；47 个 `library_busy`、17 个 `cache_invalid` 明确拒绝，观察丢弃为 0，持锁关闭历史库 103.54ms。

上述首块/回显值包含合成上游等待和客户端测量，不是 P07 的代理内部附加延迟，不能替代其阈值验收。没有声称这次触发了 3 秒调用超时。

## 5. 可复现入口

```sh
cargo test --lib g1_body_budget_preserves_all_small_metadata_and_source_windows
cargo test --lib checkpoint_reuse_preserves_metadata_when_the_new_body_share_is_full
cargo test --lib history::library::fault_tests -- --nocapture
cargo test --lib g1_scale_smoke -- --ignored --nocapture --test-threads=1
cargo test --lib g1_scale_10000_sessions_200000_items -- --ignored --nocapture --test-threads=1
cargo test --lib g1_default_cache_pressure_is_explicit -- --ignored --nocapture --test-threads=1
cargo build --bin codex-view
WORKBENCH_SCALE_SESSIONS=100 cargo test --lib binary_large_history_chrome_cold_and_warm_metrics -- --ignored --nocapture
cargo test --lib binary_large_history_chrome_cold_and_warm_metrics -- --ignored --nocapture
cargo test --lib history_library_pressure_keeps_sse_pty_and_durable_recording_live -- --nocapture
```

规模和浏览器测量串行执行，避免编译或另一轮大规模测试污染结果。浏览器测试只读首页、展开设置和阅读历史，断言零 CLI 启动、零外部请求、零终端 WS，并清理自己启动的服务和 Chrome。

## 6. 最终检查与交付

- `cargo test --all-targets -- --test-threads=4`：436 通过、0 失败、51 忽略（上述显式 opt-in 规模/Chrome运行另列，不把全部忽略项当作已验收）。日志 `/tmp/r6-g1-final-tests.log`。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`git diff --check`、TypeScript `tsc --noEmit`、Node probe语法及三份改动文档链接/代码块检查通过。
- 首次Clippy指出压力测试里一个等价整数比较写法，已按建议修正并复跑：压力专项1通过，47个队列忙、17个SQLite锁错误、观察丢弃0；`/tmp/r6-g2-final-pressure.log`。没有通过禁用lint降低门槛。
- `cargo build --bin codex-view` 通过，`target/debug/codex-view` 包含本轮修复。未改前端产品源码，原有已构建资源仍有效；没有替换用户正在运行的实例，也未启动留存的调试服务。
- 分支 `codex/native-cli-live-workbench`，已验收 A–F.1 提交 `466dff7`；本轮验证完成时G修改保留在工作区；后续按用户要求，与G2一并纳入本地提交 `fix: preserve history metadata under cache pressure`，未推送。无配置格式、派生库版本或原始历史迁移。

## 7. 保留的边界

- 正文预算耗尽后，未变化文件的 metadata-only 检查点仍可复用；后续仅增加预算或删除其它会话不会自动补回这些正文/搜索缓存。按需阅读可用，完整缓存重建策略需后续单独验证。
- 本轮尚未把查询队列实际峰值、独立索引线程 CPU、真实旧库正文抽查、完整 P07 对照、多 Run 全规模故障矩阵计为通过。2026-09-25 后续 [G2 验证](native-cli-r6-g2-isolation-2026-09-25.md)已补齐 P07 同测点压力对照、队列 Full 触顶证据与四 Run 超容量/输入隔离；其余缺口仍以实施计划为准。
- 单次 debug 样本不构成全负载性能保证。整片 R6-G 不提前标记 Passed。
