# 公网工作台中转本地集成验证（2026-10-10）

本报告记录 [ADR 0056](../decisions/0056-public-workbench-relay.md) 的本地实现与合成验收，不代替 Linux、公网 TLS 或真实手机验收。阶段状态只维护在 [V2 实施计划](../v2-implementation-plan.md)。

## 实现与环境

验证时工作分支 `codex/public-relay`，基线 `c6e8e2b`；本次为未提交的本地变更，没有部署、推送、Issue 或 PR。macOS / Apple Silicon，Rust 1.95.0、Node 22.23.2、系统 Chrome 154.0.8037.99。Node 使用官方发行包并核对 SHA-256；未将运行时或生成产物加入 Git。

该历史基线包含 V3 实验代码，以下测试数量不能代表后续独立的中转站分支。2026-10-13 用户要求隔离分支后，中转站改为基于远端 V2 `main`；重新验证及分支归属见 [IP 测试报告](public-relay-ip-trial-2026-10-13.md#分支隔离复验)。

新增独立 `codex-relayd`、共享协议及本机出站连接器。账号和归属进入独立 SQLite schema 1；既有工作台历史没有 migration。远程请求由私有 ingress 建立 RemoteOwner，连接器和 Web 共用 `workbench::remote_access`，不复用本机 Cookie。绑定后仍默认关闭，用户明确启用才连接。门户和本机均可撤销工作台；本机忘记时先失效本地许可、保存未绑定状态，再进行一次有时限的服务器撤销。

所有账号、凭据、文件与 CLI 均为合成夹具。Application 使用独立临时 HOME/CODEX_HOME/数据根，合成 CLI 通过真实 PTY 工作，不访问真实用户会话。网络仅使用 loopback；浏览器使用专属临时 profile，没有复用用户 Chrome 会话。

## 检查结果

| 检查 | 结果与范围 |
| --- | --- |
| `node scripts/dev.mjs doctor` | 通过，固定 Rust/Node 版本一致 |
| `node scripts/dev.mjs build` | 通过，先构建网页，再构建工作台、兼容服务和中转服务三个 binary |
| `node scripts/dev.mjs check --offline` | 通过：80 项工具、278 项前端、763 项 Rust 测试；67 个需显式环境的既有专项未运行，不计作通过。类型、网页构建、格式、严格 Clippy 及三个 binary 的 debug/release 构建通过 |
| `node scripts/dev.mjs e2e` | 10 项通过，复用系统 Chrome，无浏览器下载；产物 `target/artifacts/3042a3f6-c9e5-41ad-8434-bc824aab1436` |
| 中转服务专项 | 14 项通过，账号/凭据、跨账号路由、Origin、代理 IP、SSE/WS、代际、撤销、并发准入、私有文件、schema、审计与过期元数据有界清理 |
| 本机连接器专项 | 11 项通过，状态权限、TLS/loopback 配置、真实私有 ingress、断线撤销与忘记服务端凭据 |
| 最大终端快照 | 通过，200×500 彩色 VT、组合字符与 1 MiB replay 的 JSON/Base64 大于 2 MiB 且小于独立终端帧上限 16 MiB |
| 独立安全审查 | GPT-6 静态只读审查：修复准入、帧预算、重复头、私有数据库和撤销链路后，未发现新的 P1/P2 阻断项；不等同于渗透测试 |

标准检查的最终受管产物为 `target/artifacts/dc92467a-33f1-4498-8cc2-865a2a61d580`；启动撤销竞态修复前的整仓检查为 `target/artifacts/0bc966e8-239e-48ca-b494-e2d4106f6a38`。前次检查产物 `target/artifacts/f0873051-47e1-45c6-9e6b-cb9499874026` 保留真实 Clippy 失败记录，不计作通过。最终文档检查 126 个文件无错误；标准二进制检查针对空暂存区，另使用同一内容分类器检查全部 58 个变更/新文件，无禁止二进制或编码产物。`git diff --check` 通过。

## 真实中转链路与 Chrome

执行以下专项时，测试启动实际 Axum 中转、两个独立 Application、本机连接器和系统 Chrome；不拦截或模拟浏览器业务网络：

```sh
CODEX_RELAY_CHROME_TEST=1 cargo test --locked --offline \
  -p codex-local-observer --lib public_relay_routes -- --nocapture
```

测试环境须使用固定 Node、已安装系统 Chrome；可通过 `WORKBENCH_TEST_CHROME` 指定二进制。Chrome 专项最后一次执行通过，耗时 51.08 秒；随后启动受理竞态修复由最终整仓检查中的实际中转 HTTP/PTY/SSE 测试及新增并发回归覆盖，未重复无前端变更的浏览器专项。Chrome 输出只保留验证计数，不含 Cookie 或正文：

```json
{"stage":"passed","chrome":"154.0.8037.99","mobileWidth":390,"portals":2,"workbenches":2,"assetResponses":[8,3],"runs":1,"crossAccountDenied":true}
```

链路检查包括：两账号分别注册/登录/绑定、明确启用、一次性工作台会话交换；各自首页、嵌入资源、历史目录与身份；跨账号打开和移植 Cookie 被拒绝，私有握手/模型/旧入口不可公开；空闲 31 秒后续租仍有效；陈旧 instanceId 和设置 CAS 被拒绝；远程检查目录、异步启动并查询 operation；本机和远程读取同一 Run、设置、文件和历史；SSE 与终端 WebSocket 的 claim/input/ACK；注销关闭已存在的 SSE/WS，另一个账号仍可访问；本机忘记实际删除服务器登记并撤销浏览器会话。撤销和服务器终止后，本机 Application 与已启动合成 CLI 继续工作。

390px 手机视口验证两个门户和对应工作台、路径输入流程、不调用本机目录选择器、不显示本机绑定管理、设置及运行终端就绪。初始 Chrome 探针在注入 Secure `__Host-` Cookie 时被 CDP 拒绝；夹具改用同 host/port 的 HTTPS Cookie 设置地址，实际 loopback 请求仍必须发送相同 Secure Cookie 才可通过，未修改产品 Cookie 限制。该特殊夹具不证明公网 HTTPS 的证书、DNS 或代理配置已通过。

启动撤销时序复核还发现 HTTP 初次鉴权与 Book 加锁间的竞态，已在锁内加入受理前权限复查。真实线程与持锁回归覆盖“先通过鉴权、等待锁时撤销、无 operation/无入队”和“已受理 operation 在撤销后仍可查询”。独立审查确认修复对应原问题，不以同步代码没有 await 作为跨线程安全依据。

## 尚未验证的交付条件

没有实际 Linux 服务器、DNS、证书或手机公网连接，本次没有对外部署。Caddy/systemd 为文本模板，Linux 构建与服务运行、wildcard 证书签发/续期、真实移动网络的锁屏/断网/重连、服务器和本机重启、输入 ACK 丢失、完整原版 Codex 审批与图片操作，以及停机备份/恢复演练需独立留存实测证据。既有 V2 验收不自动覆盖这些新增条件。

测试只证明受覆盖的合成权限和传输路径，不证明真实模型对话、生产吞吐或真实手机体验。服务为可信中转，无端到端加密；账号归属不提供操作系统沙箱。最小下一步是在 Linux 测试主机按[部署指南](../guides/public-relay-deployment.md)启动服务和 TLS，用两个受邀测试账号、两个隔离工作台和真实手机执行剩余验收。
