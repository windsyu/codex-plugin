# 中转账号删除与管理员改名验证（2026-10-14）

[验证索引](README.md) · [ADR 0061](../decisions/0061-account-deletion-admin-rename.md) · [V2 实施计划](../v2-implementation-plan.md)

## 范围与版本

在 `codex/public-relay`、`fa094c2` 加未提交工作树改造上实现普通账号删除及默认管理员 `windsyu`。保留此前单端口、连接恢复与命令认证改造；不提交/推送、不删除真实测试账号、不重启本机正在运行的 CLI。

删除权限按角色而非名称判断。事务清除普通账号的会话、凭据和电脑登记，保留审计并关闭业务流；同名重建为新 UUID。schema 3 改名保留管理员 UUID/密码/状态/代际/电脑归属，有冲突则整个迁移回滚。

主要改动位置：账号删除受理与路由在 `relay/src/admin.rs` / `lib.rs`，改名迁移和启动/恢复在 `relay/src/store.rs` / `main.rs`；门户在 `relay/static/portal.js` / `portal.css`。回归在 `relay/src/account_api_tests.rs`、`account_store_tests.rs`、`tests.rs`、`web/src/workbench/portal.test.ts` 和 `web/e2e/relay-account-probe.cjs`；跨模块夹具与测试诊断助手分别修正 `src/workbench/application/tests/relay.rs` 和 `src/history/library/scale_tests.rs`。ADR、契约、部署指南及 V2 计划同步更新。

## 本地验证

- `cargo test --locked -p codex-relayd`：52 个 library、1 个 binary 测试通过。覆盖权限/Origin/自身及其他管理员保护、关联清除、审计失败全回滚、管理员会话并发撤销、迟到登录/批准/重连、活跃 SSE/终端关闭、schema 1→3 与 2→3 冲突及幂等迁移。
- `cargo clippy --locked -p codex-relayd --all-targets -- -D warnings` 通过，formatter 通过。
- 门户单元测试先复现缺少删除入口（4 项失败），实现后 16 项通过；覆盖取消、204 成功、503 失败和退出后的迟到响应。
- `node scripts/dev.mjs build` 通过，先生成前端再构建二进制；产物任务 `bf4c28fc-28c8-4119-920d-7d66d4219b14`。
- 第一轮标准 check 的维护工具 85 项、前端 300 项、TypeScript、fmt/Clippy 通过；两项跨模块夹具仍使用旧管理员名，已修正为 `windsyu`，专项 3 项全部通过。第一轮任务 `a333be17-46da-4de9-b999-81dc91f164da` 保留失败证据；最终标准 check 另列。
- 第二轮标准 check：维护工具/前端/fmt/Clippy 通过，主库 386 通过、1 失败、52 ignored。失败仅为既有规模测试的诊断统计：目录枚举后的 SQLite 临时 journal 已移除，`metadata().unwrap()` 收到 NotFound。已修正测试助手仅容忍文件消失，其他错误仍失败，新增确定性回归，原业务断言全部保留；任务 `3efafe1a-2e92-4b10-956a-1a586f5e7bb6` 保存失败证据。此改动为测试代码，不改变已构建中转包。
- 最终 `node scripts/dev.mjs check` 完整通过，任务 `a7f3e1eb-4ee1-48b8-b62d-11af94bee9b6` 的登记结果为 success：维护工具 85 项、前端 300 项、TypeScript、Web 构建、workspace fmt/Clippy、全部常规 Rust target 及 debug/release binary 构建通过。主库 388 项、中转 library 52 项和 binary 1 项通过；14 个 Rust target 合计 546 次通过、57 次 ignored，后者未执行，不计入通过结论。确定性诊断回归和原历史规模测试在最终集成状态均通过。
- `node web/e2e/relay-account-probe.cjs` 使用系统 Chrome 154.0.8037.100，390px 手机视口，隔离合成数据：首次改密/重新登录、显示 windsyu、普通 admin 名称不提权、取消不发 DELETE、模拟 503 保留行、确认删除和旧会话 401、同名重建新 UUID、恶意名称安全文本及无横向溢出全部通过；0 页面错误。任务 `33d1de3d-1990-4b08-9b7f-a1d6bbd264e0`。已验证专用 relay/Chrome 退出，删除的仅为该脚本自己的合成运行目录。
- 首次 sandbox 拒绝 loopback bind；获准执行后通过。Chrome 探针初次 Node fetch 未保留虚拟 Host，测试助手改用 `node:http` 后最终脚本通过；不降低产品权限门槛。
- 收尾文档检查 123 项、`git diff --check` 通过。二进制策略检查执行成功，但暂存区为空（checked 0），不以此宣称整个未提交工作树或历史已完成二进制审计。

独立 Astra 审查未发现产品认证/迁移可行动缺陷。部署审查发现包校验不能证明服务用户执行权限，已在停服前补 `test -x` 与限时 `--help`，复审通过；部署保护的 16 项合成测试和 `bash -n` 通过。审查不是正确性证明，未验证范围仍单列。

## Linux 分发与升级准备

静态 `x86_64-unknown-linux-musl` 分发任务 `364f334e-075a-4887-9399-8de4dbe6833f`，先生成前端，再 Cargo `--locked --offline` 构建；292 项许可包含前端及四类实际静态运行库。去除 macOS 扩展元数据后的归档检查 3,864 个成员、3,337 个文件、路径/类型/mode/字节/全部校验和与静态 ELF 通过。

| 产物 | SHA256 |
| --- | --- |
| binary | `a50656c0f208f09b9b4e038ece910e57f74e4a7287b2d5314a52a9f963088cfd` |
| clean archive | `b9a8055fe86094c13ba39dd11eadc2333c8f7604d8ebcc8b803fa203ae49e88f` |
| manifest | `d9ac713faadcfceecfef674cb90a9968aa9f01207c6d081dbd7ecc7c85b3af37` |
| checksums | `ae3e86c4f389cd9437bcc48aa70fe6d0e79f66a50f7944222d7406cf21d35e1f` |

指定 ECS `i-bp16nvvpdk423m48yewl` 升级前只读检查：原服务 active/running，schema 2、integrity `ok`、外键无异常，3 账号/1 电脑，`windsyu` 无名称占用；8 个业务容器和专用 TLS 容器正常运行。暂存包校验与远端 preflight 通过，任务 `29edf065-0270-4bb2-a82d-3dd19cb9e784`。实际 apply 结果另列，不以暂存或 preflight 代替部署成功。

升级仅作用于专用 `codex-relayd-ip-trial.service`。停服后保存全目录与 SQLite 一致性快照，加密/HMAC 备份并恢复到新的私有副本目录验证；不覆盖在线库。schema 3 后失败只停服/保全当前状态，不恢复旧快照或旧二进制。启动后的账号/电脑并发变更可能触发严格比较的安全停服，应选择安静升级窗口。

## 远端上线结果

最终标准检查通过后执行指定 ECS 的 apply，任务 `6916af87-62ee-47c8-b152-4d2b163cbddf` 成功。先验证服务用户可执行新 binary，再停止专用服务、创建加密/HMAC 备份、实际解密恢复到新私有副本目录并核对，最后切换 binary 启动。启动就绪轮询最初一次遇到 loopback 尚未监听，随后本机及公网路由健康门槛均通过；未触发失败恢复。

- 运行 binary SHA256 与上表一致；schema 3、integrity `ok`、外键无异常。迁移前后逐字段比较确认仅旧默认管理员登录名变化，UUID、密码哈希、状态、代际及电脑归属/凭据保持不变。
- 服务 active/running，PID `1416666`，自动重启计数 0；3 个账号、1 台电脑保留，`windsyu` 管理员计数 1，旧 `admin` 管理员计数 0。未删除任何真实测试账号。
- 私有备份位于 `/opt/codex-relay-trial/backups/364f334e-075a-4887-9399-8de4dbe6833f-schema2-to3`；密钥仅留服务器私有目录，不写入日志或仓库。没有恢复在线数据库、重新 bootstrap 密码或回退旧版。
- 公网 HTTPS 资源验证任务 `1d64f0f3-9533-460e-b4a3-8f8bb4a1f95f` 成功：TLS 校验开启，门户 200，匿名账号/电脑/管理 API 均为 401；门户 JS/CSS 和全部 6 个构建静态资源逐字节一致。
- 系统 Chrome 使用既有登录打开账号管理页，无须重新输入密码，显示 `windsyu`；两条原有普通账号行均出现“删除账号”按钮，原禁用/启用状态保留。仅只读核验，未点击真实删除、重置、禁用或创建。
- 8 个业务容器与专用 TLS 容器运行时长未重置，既有 443 入口仍为 200；资源限额保持 CPU 350ms/s、内存 384 MiB、TasksMax 64。本机工作台 PID `69596` 及 launcher `69595` 仍运行，未重启或停止。

## 限制

真实手机/移动网络、公网真实模型消息与多电脑完整 V2 仍须独立验收；Chrome 手机视口和合成账号测试不能代替真机。电脑凭据中的旧用户名缓存只是展示，不作为归属依据；重新认证会更新名称。未修改原生 Codex 配置/历史，也未自动删除截图中的账号。
