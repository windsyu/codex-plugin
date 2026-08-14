# Codex Local Observer

本机只读的 Codex rollout 历史观察器。当前版本实现 V1 store-first MVP：从一个或多个 `CODEX_HOME` 导入 durable JSONL 历史，投影为 Thread → Turn → Item，并通过带认证的本地 API 和 Web Viewer 查询。

## 已实现

- plain `.jsonl` 与 cold `.jsonl.zst` 扫描；
- 文件系统 watcher 触发 200ms debounce rescan，并保留周期全量扫描兜底；
- active / archived rollout 发现和重复导入幂等；
- EOF 半行保留、坏 JSON 审计占位、unknown variant 无损保存；
- raw event、Thread/Turn/Item projection、trigram FTS 和 checkpoint 同一 SQLite 事务提交；
- 已知 secret key、credential prefix 和环境变量值入库前脱敏；
- Thread、Turn、Item、event、search、health、source 和 capabilities REST API；
- SSE 与 WebSocket committed-event 续传；
- bearer token、loopback-only、Origin 检查、CSP 和纯文本 Raw Inspector；
- `serve`、`import`、`doctor`、`rebuild-projections`；
- Observer 数据库 writer 使用进程级 advisory lock，拒绝并发写实例；
- 合成 fixture，不读取或提交真实用户 rollout。

当前 `live_mode` 强制为 `off`。App Server Live Adapter、blob 外置、retention/purge/export、签名 cursor 和完整性能加固属于后续 V1 切片，详见[详细设计](docs/codex-local-observer-detailed-design.md)。

## 构建与测试

需要 Rust 1.95 或兼容版本：

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

测试只使用临时目录和 `fixtures/` 中的合成数据，不会写入 `~/.codex`。

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

所有相对路径以配置文件所在目录为基准。V1 拒绝非 loopback bind 和非 `off` live mode。

## 使用

先运行只读诊断和一次导入：

```bash
cargo run -- doctor
cargo run -- import
```

启动周期扫描、API 和 Viewer：

```bash
cargo run -- serve
```

然后打开 `http://127.0.0.1:4765`，粘贴 `observer-data/token` 中的 bearer token。token 和 fingerprint key 首次运行时生成，Unix 权限为 `0600`。

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

V1 当前注册的接口全部为 `GET`：

```text
/v1/health
/v1/sources
/v1/threads
/v1/threads/{threadKey}
/v1/threads/{threadKey}/turns
/v1/threads/{threadKey}/items
/v1/threads/{threadKey}/events
/v1/events
/v1/search?q=...
/v1/meta/capabilities
/v1/stream
/v1/stream/ws
```

WebSocket 连接后需在 5 秒内发送订阅 frame；当前实现接受：

```json
{"type":"subscribe","afterEventSeq":0,"filters":{}}
```

## 安全边界

- Observer 只读打开 Codex rollout，不修改 Codex SQLite、rollout 或 writer lock；
- Web Viewer 仅渲染已脱敏副本，JSON 使用 `textContent`，不执行 Markdown、HTML、SVG 或 ANSI；
- fingerprint 使用本机随机 256-bit key 的 BLAKE3 keyed hash；
- API Router 不存在 POST/PUT/PATCH/DELETE 业务路由；
- 当前 completeness 仅声明 durable coverage，不声称恢复未连接时的 transient event。

## 项目状态

本目录尚未初始化 Git。根据项目工作流，后续应初始化仓库、创建首个 Issue 和 feature branch，再提交此纵向切片；远程仓库、push 和 PR 需要用户明确授权。
