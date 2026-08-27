# Codex Local Observer

本机只读的 Codex rollout 历史观察器。当前版本实现 V1 store-first MVP：从一个或多个 `CODEX_HOME` 导入 durable JSONL 历史，投影为 Thread → Turn → Item，并通过带认证的本地 API 和 Web Viewer 查询。

## 已实现

- plain `.jsonl` 从 checkpoint byte offset 流式续读，cold `.jsonl.zst` 按 logical ordinal 流式重放；专用 DbWriter 按最多 100 event / 50ms 合并事务；
- 文件系统 watcher 触发 200ms debounce rescan，并保留周期全量扫描兜底；
- active / archived rollout 发现和重复导入幂等；
- EOF 半行保留、坏 JSON 审计占位、unknown variant 无损保存；
- 单行读取受 `max_raw_event_bytes` 约束；oversize 只保存完整输入 fingerprint 和审计占位，不会阻塞后续 record；
- raw event、Thread/Turn/Item projection、trigram FTS 和 checkpoint 同一 SQLite 事务提交；
- SessionMeta 的 parent/fork/sub-agent/history-base 与每 Turn model/effort/approval/sandbox/permission context 结构化投影；SessionMeta `base_instructions`、dynamic tools、capability roots、memory mode 与 context window 也作为线程背景上下文保存；
- Web Viewer 按规范化 `cwd` 聚合项目，使用接近 Codex 的紧凑项目/会话侧栏与居中对话画布；用户/助手消息保持主阅读流，命令、工具、文件、reasoning、usage 与兼容诊断按 Turn 折叠汇总；
- Web Viewer 使用 Vite + Preact；Cookie 模式使用 SSE，bearer 模式安全降级为轮询；Markdown 经 DOMPurify 清洗，代码块按需注册 highlight.js 语言，脚本和本地文件链接不作为可执行内容；
- redaction v2 在入库前处理 secret/header/MCP auth、URL token/signature，并以不可还原 marker 丢弃 image/audio base64 正文；
- Thread completeness 从全部 Turn coverage 聚合，clean EOF、decode/unknown/disconnect 计数不会再被后续事件清零；
- Thread、Turn、Item、relation、event、search、health、source 和 capabilities REST API；
- Thread、Turn、Item 与 Search 列表支持签名 keyset cursor、稳定 `asOfEventSeq`，游标绑定端点及筛选条件；
- Viewer 自动消费 Thread/Turn/Item/Search cursor 与 raw event sequence，不静默截断长时间线；
- 4096-event 有界 ingest 背压、512-slot CommittedEventBus，以及 SSE/WebSocket 的 DB replay → live 无缝切换和慢消费者隔离；
- `serve` 每次启动轮换本机 token，并打印在该次运行期间稳定、可重复使用的本机配对 URL；可选在同一次启动中建立 Tailscale Serve HTTPS → loopback 转发并打印固定 Tailnet URL；
- `serve`、`import`、严格只读 `doctor`、`rebuild-projections`；
- `retention` 默认 dry-run，`--apply` 删除过期 raw，但保留 projection、dedupe tombstone 和 cursor low watermark；
- `export` 在 writer lock 前分流，用只读 WAL snapshot 输出独占创建的 `0600` 脱敏 JSON；daemon 运行中也可导出；
- 超过 `inline_blob_bytes` 的已脱敏 raw JSON 使用内容寻址 blob 原子落盘；投影保存引用，支持 orphan sweep、引用感知 retention 和安全 Range 下载；
- Observer 数据库 writer 使用进程级 advisory lock，持有唯一写 connection；查询使用最多 8 条 read-only connection；
- Observer 数据目录为 `0700`，数据库/WAL/SHM、lock、token、key 和 blob 为 `0600`；当前用户拥有的旧宽松 mode 会自动收紧；
- 可选 App Server Live Adapter：Unix WebSocket、稳定版 initialize、周期 archived/non-archived list/read 对账、`observe_new` / `attach_loaded`、断线抖动退避重连；破坏性协议不兼容会 fail closed 到 store-only；
- live notification、response 和 server request 先入 raw event，再更新运行态投影；approval/question 只展示，Observer 永不响应；
- live epoch 连续性/连接统计、capability/schema fingerprint、pending/resolved request 和断线 stale 状态持久化；durable/live 不一致会写入 `projection_conflicts`；
- 合成 fixture，不读取或提交真实用户 rollout；
- 10,000 Thread + 10,000 Item/FTS 查询规模冒烟测试；容量 SLA 按详细设计在更大原型数据集测量后冻结。

`live_mode` 默认仍为 `off`，开启后属于 opt-in preview；`attach_loaded` 会调用官方 `thread/resume`，可能影响 Thread loaded 生命周期并触发上游恢复行为。Store-first durable history 仍是正确性主链路。更大数据集的容量规划与 SLA 冻结属于后续发布工程，不影响当前 V1 MVP 边界，详见[详细设计](docs/codex-local-observer-detailed-design.md)。

## 构建与测试

需要 Rust 1.95 或兼容版本：

```bash
cd web
npm install
npm test
npm run test:e2e
npm run build
cd ..
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

测试只使用临时目录和 `fixtures/` 中的合成数据，不会写入 `~/.codex`。
`cargo build` 会检查 `web/dist/index.html` 是否已生成；缺失时会提示先完成前端构建。
发布前自动化、release E2E、安全与容量冒烟结果见 [V1 验证记录](docs/v1-validation.md)。

## 配置

复制示例配置，但不要把本机配置、token 或数据库提交到仓库：

```bash
cp observer.example.toml observer.toml
```

默认配置在未找到 `observer.toml` 时也可使用：

- API：`127.0.0.1:4765`；
- Codex source：`~/.codex`；
- Observer 数据：`./observer-data`；
- Live Adapter：关闭。

所有相对路径以配置文件所在目录为基准。V1 拒绝非 loopback bind；live 仅支持当前用户拥有、权限不宽于 `0600` 且位于私有目录中的直接 Unix socket。

若所有访问设备已经加入同一个 Tailnet，可在保持 Observer loopback-only 的前提下启用启动期端口转发：

```toml
[server.tailscale_serve]
enabled = true
https_port = 443
```

该模式要求本机 `tailscale` CLI、daemon、MagicDNS 和 HTTPS 已启用。Observer 启动时只创建根路径 `https://<machine>.<tailnet>.ts.net[:port]/ → http://127.0.0.1:4765`；已有不同 Serve 配置时拒绝覆盖，且永不启用 Funnel。Tailscale HTTPS 会把机器 FQDN 写入 Certificate Transparency，机器名不得包含敏感信息。

`capture.ingest_queue_events` 与 `capture.api_consumer_queue_events` 默认分别为 4096 和 512。`keep_reasoning=false` 会只保留 reasoning 身份与 policy marker；`keep_raw_json=false` 会保留 raw event 行与 checkpoint，但不持久化 raw 正文，projection 仍使用入库前的已脱敏内存结构。`delta_retention_days` 独立控制 transient delta，不再沿用普通 raw retention。

需要显式启用 live preview 时，在对应 source 中配置：

```toml
app_server_socket = "~/.codex/app-server-control/app-server-control.sock"
live_mode = "observe_new" # 或 attach_loaded
```

两种模式每 `max(scan_interval_seconds, 30s)` 分别对 archived/non-archived Thread 做 list/read 对账；`attach_loaded` 还会复查 loaded 集合，并且只对该连接首次发现的 loaded Thread 调用 `thread/resume`。两种模式都不会发送 approval、question、turn 或其他控制响应。

## 使用

先运行只读诊断和一次导入：

```bash
cargo run -- doctor
cargo run -- import
cargo run -- retention
cargo run -- retention --apply
cargo run -- export --thread '<threadKey>' --output ./thread-export.json
cargo run -- purge --thread '<threadKey>' --observer-copy-only --yes
```

`doctor` 不获取 writer lock、不创建数据库或 blob 目录、也不执行 migration。`export` 拒绝覆盖已有文件及写入任一 Codex source。`purge` 仅删除 Observer 数据；抑制墓碑会阻止后续 rescan/live 自动恢复该 Thread，Codex rollout 保持不变。

启动周期扫描、API 和 Viewer：

```bash
cargo run -- serve
```

服务成功监听后会打印 `Local Viewer: http://127.0.0.1:4765/#pair=...`。该配对 token 在本次 `serve` 运行期间保持不变，可由多个本机应用重复兑换；Viewer 兑换为 `HttpOnly; SameSite=Strict` Cookie 后立即清除 fragment，URL 不直接包含 bearer secret。下一次 `serve` 启动会轮换 token，使旧配对 URL、Bearer token、Cookie 和签名 cursor 失效。

启用 Tailscale Serve 时还会打印固定的 `Tailscale Viewer: https://<machine>.<tailnet>.ts.net/`。远程浏览器不需要配对 token：Serve 先执行 Tailnet ACL，再向 loopback 后端注入已验证身份；Viewer、API、SSE、搜索和 Blob 下载继续走与本机完全相同的实现。

需要再次复制当前启动的同一个链接时，可在 daemon 运行期间从另一个终端执行（命令只打印，不自动打开，也不轮换 token）：

```bash
cargo run -- open
```

Bearer token 仍保留给其他本机应用、CLI/API 和 Viewer 高级故障恢复。`serve` 每次启动时原子替换 token 文件；fingerprint key 仍只在首次运行时生成。两者的 Unix 权限均为 `0600`。

使用 API：

```bash
TOKEN="$(tr -d '\n' < observer-data/token)"
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4765/v1/health
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:4765/v1/threads
```

仓库内的合成 fixture 可用于演示：

```bash
cargo run -- --config fixtures/observer.fixture.toml import
cargo run -- --config fixtures/observer.fixture.toml serve
```

演示 Viewer 地址为 `http://127.0.0.1:4766`，生成数据位于被 `.gitignore` 排除的 `target/fixture-observer-data`。

## API

V1 业务查询接口为 `GET`；唯一 POST 是本机 Viewer 的启动级 token 配对兑换：

```text
/v1/health
/v1/sources
/v1/projects
/v1/threads
/v1/threads/{threadKey}
/v1/threads/{threadKey}/turns
/v1/threads/{threadKey}/items
/v1/threads/{threadKey}/events
/v1/events
/v1/blobs/{blobId}
/v1/search?q=...
/v1/meta/capabilities
/v1/meta/settings
/v1/stream
/v1/stream/ws
/v1/auth/pair (POST)
```

`/v1/threads`、`/turns`、`/items` 与 `/search` 使用不透明的签名 `cursor`；客户端应原样传回响应中的 `nextCursor`。`items` 还支持 `turnId`、`itemType` 筛选，`threads` 支持 `project=<projectKey>` 项目筛选。游标不能跨端点、Thread 或筛选条件复用。

WebSocket 连接后需在 5 秒内发送订阅 frame；当前实现接受：

```json
{"type":"subscribe","afterEventSeq":0,"filters":{}}
```

`filters` 可包含 `threadKeys`、`sourceIds`、`methods` 数组，每类最多 100 个值。发送阻塞超过 10 秒时服务端尝试返回 `SLOW_CONSUMER` 并关闭连接；客户端应从最后收到的 `eventSeq` 重连。

## 安全边界

- Observer 只读打开 Codex rollout，不修改 Codex SQLite、rollout 或 writer lock；
- Web Viewer 仅渲染已脱敏副本；Markdown 经过 DOMPurify 清洗，禁止脚本/iframe/style 与本地文件链接，JSON 仍使用纯文本展示；
- blob 路径完全由服务端生成，下载强制 attachment，单 Range、并发上限 4；文件使用 `O_NOFOLLOW` 打开；
- fingerprint 使用本机随机 256-bit key 的 BLAKE3 keyed hash；
- redaction v2 不自动重写历史 v1 记录；health、Viewer 和 export 会报告 legacy record 警告；
- API 不存在控制 Codex 的 mutation 路由；`POST /v1/auth/pair` 只兑换 Observer 会话 Cookie；
- Tailscale 模式仍只监听 loopback；仅接受匹配 MagicDNS Host、HTTPS 转发标记和 Serve 用户身份的请求，不支持 Funnel 或普通 LAN 暴露；
- Store completeness 仅声明 durable coverage；live completeness 只有在同一连续 epoch 观察到 Turn started 和 terminal 时才标记完整；
- App Server transport 仍为官方实验能力，因此默认关闭，协议不兼容时退回 store-only。

## 项目状态

本地 Git 已初始化，当前 V1 开发在 feature branch 以逻辑提交维护。仓库尚未配置 remote；创建远程仓库、push 和 PR 仍需单独授权。
