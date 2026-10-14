# 原生命令认证接入验证（2026-10-14）

对应 [ADR 0060](../decisions/0060-native-provider-command-auth.md)。本记录区分配置接入、安装版合成回归和真实 provider；中转登录不等于模型认证成功。阶段状态以 [V2 实施计划](../v2-implementation-plan.md)为准。

## 检查范围

分支 `codex/public-relay`，基础 commit `fa094c23b3742a6bb6cb475c7ac924fd7357582f`；命令认证及既有公网增量均为未提交工作树。自动化使用隔离的 `HOME`、`USERPROFILE`、`CODEX_HOME`、合成认证脚本与 loopback upstream；没有读取或执行真实用户的认证命令，没有发送真实模型请求，也没有修改远端服务。

`Profile::load` 现区分静态 bearer 与命令认证。命令路径校验非空 `auth.command`、字符串数组 `args`、`requires_openai_auth=false` 及冲突项；命名 profile 的 `auth` 成员按原生递归覆盖语义从 base 补全。空值或错误类型的静态 bearer 也按字段存在冲突拒绝，错误不包含命令、参数或凭证正文。

命令 token 仍完全由官方 CLI 执行、缓存和刷新。工作台保留原 File credential store、非 cloud/ChatGPT 登录、系统/managed 来源门禁和 `auth.json` 启动快照，因为原生登录会独立加载可能补入 provider 字段的 cloud config；该快照不提供命令 token。模型代理不改写 `Authorization`，观察源只保留正文及既有的 `Content-Type`/`Content-Encoding`，不记录完整请求头。

## 回归测试与结果

| 检查 | 所覆盖的断言 | 结果 |
| --- | --- | --- |
| `workbench::launch::profile::tests` | 两种认证互斥；空白 command/错误 args/静态 bearer/env/AWS/true 冲突拒绝；File API Key/登录快照；命名 profile 的 args-only 与 refresh-only auth 覆盖；错误不泄漏配置正文 | 6 passed |
| `workbench::config_sources::tests` | system/managed/cloud/非 File 来源门禁；认证文件缺失/变化/坏编码/超限/symlink/目录/FIFO；来源诊断不含 secret | 7 passed |
| `workbench::redaction::tests` | 已知秘密、常见 Bearer/API key/private key/data URI 跨分片脱敏；Bearer 字符集覆盖 `~` | 5 passed |
| `product_homepage_leaves_command_auth_and_401_refresh_to_native_cli`（ignored） | 工作台预检不执行命令；CLI 首次 bearer 收到 401 后重跑命令；并发旧 bearer 均保持 401，刷新后的 bearer 完成模型目录与 SSE；Chrome 新建/恢复两轮、同一原生 thread、零自动输入；journal 不含两个 token 或命令名 | 1 passed |

安装版专项使用 `codex-cli 0.162.1`、系统 Chrome `154.0.8037.100` 和刚由标准 build 生成的 debug `codex-view`。浏览器探针报告 `starts=2`、`sameNativeThread=true`、`zeroAutomaticInput=true`、`pageErrors=0`，并覆盖 1024/736/390/320 视口。第一次在受限命令沙箱内运行因 loopback listener 返回 `Operation not permitted` 而失败；同一测试在允许本机 loopback/系统 Chrome 的执行环境中通过，这不是产品断言失败。

执行命令：

```sh
cargo test --locked --offline workbench::launch::profile::tests
cargo test --locked --offline workbench::config_sources::tests
cargo test --locked --offline workbench::redaction::tests
node scripts/dev.mjs build --offline
cargo test --locked --offline product_homepage_leaves_command_auth_and_401_refresh_to_native_cli -- --ignored --nocapture
```

标准 build 先生成前端再构建所有 binary，artifact 为 `target/artifacts/f39ea217-f05a-4796-8fbe-6fb5a768e151`。`node scripts/dev.mjs check --offline` 在允许 loopback 的隔离环境从头通过：85 项维护脚本、296 项 Web、Rustfmt、Clippy `-D warnings`、工作区常规 Rust 测试及 debug/release binary 构建均通过；主库为 387 passed、52 ignored，旧服务 94 passed、1 ignored，中转 41 passed，其余 crate/binary/integration 通过；最终 artifact 为 `target/artifacts/b3fe8e72-b8ce-4230-a9fe-2fb21493a04e`。受限沙箱中的首次 `check` 因禁止 loopback 统一导致 94 个网络测试失败，获准复跑后没有产品断言失败。`node scripts/dev.mjs docs` 另检查 120 项、0 errors。专项 ignored 测试不计入常规测试通过数。

## 安全与剩余范围

动态 token 不被工作台读取，因此无法按精确值保证脱敏上游或工具主动放入正文的裸 token；测试确认认证头与合成 token 不进入 observation journal，但这不等价于任意秘密泄漏防护。真实 provider、真实认证命令、真实项目消息与公网手机链路没有在本记录中执行，不继承静态 bearer 或网页 200 的证据。

本增量没有数据库、relay schema 或原生历史 migration；没有提交或推送。上述隔离验证未修改远端服务；用户后续授权的配套中转更新另见[部署记录](public-relay-command-auth-deploy-2026-10-14.md)，不将部署检查当作真实模型发送证据。命令安全、stdout 格式、超时和刷新间隔继续由用户配置与官方 CLI 负责。
