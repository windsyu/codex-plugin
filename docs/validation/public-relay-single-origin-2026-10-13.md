# 管理员账号与单端口多电脑：本地验证（2026-10-13）

## 基线与范围

- 分支 `codex/public-relay`，起点 `fa094c23b3742a6bb6cb475c7ac924fd7357582f`；本报告对应其上未提交的本地改动，不代表已发布版本。
- macOS / Apple Silicon，Rust 1.95.0、Node 22.23.2。CLI 为隔离临时目录中的合成进程；没有使用私人 Codex 数据、线上中转数据库或真实模型。
- 需求与安全契约见 [ADR 0058](../decisions/0058-admin-accounts-single-origin-relay.md)、[中转契约](../architecture/relay.md)。阶段状态仅在 [V2 实施计划](../v2-implementation-plan.md) 维护。

## 已执行检查

| 检查 | 结果及范围 |
| --- | --- |
| `node scripts/dev.mjs doctor` | 固定工具链及 Apple SDK 通过 |
| `node scripts/dev.mjs build --offline` | 前端优先，三个 binary debug 构建通过 |
| `node scripts/dev.mjs check --offline` | 文档、维护工具 80 项、TypeScript、Web 250 项、fmt、Clippy `-D warnings`、Rust 504 通过 / 56 ignored、debug/release 构建通过 |
| `node scripts/dev.mjs e2e` | 系统 Chrome 常规浏览器回归 10 项通过；不下载浏览器 |
| `node --test scripts/tests/workbench-assets.test.mjs` | 新共享资源守卫专项通过：缺失任一 HTML、过期入口、过期嵌套源码均拒绝，仅新鲜资源通过 |
| 分发许可收集 | 实际 Cargo/npm 依赖 289 包许可收集通过，包含新共享资源 crate 与 Web 运行依赖；未创建发布包或远程 Release |
| 二进制策略 | 常规 staged 检查为 0 文件（本轮未暂存）；另对所有 modified/untracked 源文件使用同一内容检测器，未发现禁止二进制 |

标准检查产物登记在 `target/artifacts/660ae11e-0b2d-4340-aca7-eb6ddc0bb90f`；常规浏览器记录在 `target/artifacts/711a8550-d872-4445-9f97-0765eb0a2d92`；许可索引在 `target/artifacts/228211ea-3397-4237-b00f-e64358efae4c`。均为本地受管产物，按保留策略过期，不作为需提交的私人数据。

## 关键行为证据

- 账号测试覆盖私有密码文件/所有者/链接拒绝、默认 admin 幂等与同名冲突、首次改密权限、admin 事务复查、重置/禁用/恢复全端失效及普通用户无法管理账号。
- 迁移测试使用合成 schema 1，保留 UUID、密码哈希、电脑归属；旧会话/邀请/电脑授权失效；失败迁移回滚并拒绝未来 schema。没有修改真实用户库。
- 三个真实 Application、两个账号、同一 loopback origin：Alice 两台与 Bob 一台并行 HTTP；跨账号/跨电脑 grant 拒绝，admin 无越权；Alice 改密后两台均需重认证，Bob 不受影响；旧 grant 加新 Cookie 仍拒绝；恢复绑定保留登记和归属；撤销后本机 CLI 进程继续存在。
- 连接器 barrier 测试覆盖等待设备码轮询时取消、迟到批准、凭据保存失败不连接、成功保存后才连接、401 停止重试和重启后仍需认证。
- 单入口路由测试覆盖父 Cookie 精确绑定、选择器重复/编码/路径逃逸拒绝、SSE/WS query 选择器剥离、中央 HTML 不请求电脑页面、版本不兼容、恶意 MIME/响应头拒绝、204/304 空响应约束。域名与旧固定单机模式单独回归。
- 独立只读安全复核识别并修正 `?run=` 导航误拒、登录跨禁用/恢复代际竞态、删除电脑事务复查、门户状态字段不一致。进一步为成功登录切换撤销原父会话、并发登录消费原父会话的竞争增加回归。
- 本机停止运行接口改为 202 JSON 确认，避免操作已执行却被中央 MIME 检查判为 502；相关本地及中转集成断言覆盖。

## 增量检查

标准检查完成后，新增登录替换/CAS 回归及共享资源守卫测试，并完成以下增量验证；不将此前 504 项 Rust 结果冒充最终代码的全量重跑。

| 检查 | 结果及范围 |
| --- | --- |
| `cargo test --locked --offline -p codex-relayd --all-targets -- --test-threads=4` | 最终中转 library 38 项、CLI 1 项全部通过；包含并发登录消费父会话的 CAS 回归 |
| `cargo fmt --all --check`、`cargo clippy --locked --offline --all-targets -- -D warnings` | 通过 |
| `CODEX_RELAY_CHROME_TEST=1 cargo test --locked --offline -p codex-local-observer --lib single_origin_three_computers -- --test-threads=1 --nocapture` | 系统 Chrome 154.0.8037.100、390px 视口通过；2 个门户、3 台工作台、23 次中央资源响应、SSE 和终端 WS；跨电脑授权与账号切换均拒绝 |
| 历史域名模式 Chrome 专项 | 同一系统 Chrome、390px 视口通过；两个账号、两台工作台、运行页面及跨账号拒绝独立覆盖 |
| `node scripts/dev.mjs build --release --offline` | 最终前端及三个 release binary 构建通过，包含登录 CAS 修正 |

三电脑 Chrome 专项之后，同一集成测试继续验证改密关闭终端、原两台电脑重新认证后保留登记 ID、另一账号不受影响、本机 CLI 继续运行及类型化停止响应。浏览器探针只使用合成账号与临时存储。成功登录现在撤销请求携带的原门户会话；旧测试将管理员 Cookie 带入普通用户登录的夹具已改为模拟独立浏览器。

最终 release 构建记录在 `target/artifacts/8e6c179d-f9a0-4772-ad28-7f57561eec7c`。完整标准检查与上述增量结果应分别阅读；未执行的 ignored 专项不算通过。

## 未验证与部署门槛

本轮未执行真实手机网络/输入、Linux 新模式部署、受信任公网 IP TLS、证书续签、生产停机备份恢复、真实模型或全部 ignored 专项。Chrome 手机视口与合成 CLI 不替代这些验收。旧双端口部署证据仅对当时版本有效。

部署前必须备份 schema 1 数据库、配套升级电脑端和中转、初始化 admin 私有密码文件、重新认证原电脑，再按[运维指南](../guides/public-relay-deployment.md)执行现场验收。本轮没有提交、推送、修改安全组、安装线上服务或迁移生产数据。
