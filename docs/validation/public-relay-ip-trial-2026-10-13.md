# IP 中转测试部署（2026-10-13）

用户授权在已有重要容器的资源有限公有云服务器试用，明确使用 IP 并复用现有 Caddy SSL 证书。本报告记录实际部署、已执行检查和公网连通复验；不将服务器内部测试或桌面手机视口当成真实手机验收。[ADR 0057](../decisions/0057-single-workbench-ip-trial.md)与 [IP 操作指南](../guides/public-relay-ip-trial.md)定义本次限制。

## 环境和变更

验证时工作树为 `codex/public-relay`，基线 `c6e8e2b`，包括此前未提交的公网中继实现；验证期间未提交、推送或发布。服务器为用户指定的 Alibaba Cloud Linux 3，x86-64、2 vCPU、约 1.87 GiB RAM、无 swap，systemd 239 / cgroup v1。部署前可用内存 857 MiB、磁盘剩余约 28 GiB；原有 8 个容器运行，有健康检查的 3 个均 healthy。

新增固定 IP / 双非默认 HTTPS 端口模式，只允许一个已绑定工作台，保留账号归属和完整 V2 RemoteOwner。固定 UUID 受事务绑定与启动校验保护；未改变隧道协议或 SQLite schema 1。`--password-workers 1` 降低密码验证并发，不降低 Argon2id 参数。IP Cookie 作用域包含该 IP 上的其他 HTTPS 服务，不能宣称逐工作台主机隔离。

Linux 二进制在 Mac 的独立交叉编译目录构建，Rust 1.95.0 + 官方 SHA-256 验证的 Zig 0.14.1，`cargo build --locked --offline --release -p codex-relayd --target x86_64-unknown-linux-musl`。产物为约 4.8 MiB 的静态 x86-64 ELF，没有 PT_INTERP 或 PT_DYNAMIC；服务器 SHA-256 核验、`--version`、`--help` 通过。部署二进制摘要：

```text
22a732526e91643423a53c620541c9c973d50abfaef4b34aab4b9c002ea12c07
```

未在服务器安装 Rust、编译、拉取新镜像或修改已有站点。只创建独立用户、配置/数据目录、`codex-relayd-ip-trial.service`、`codex-relay-trial-tls` 和证书刷新单元。本机另启动新版无 CLI 首页，使用独立的配置和工作台历史目录，不自动绑定或提交模型请求。

## 自动检查与独立评审

`node scripts/dev.mjs check --offline` 使用 Node 22.23.2 / Rust 1.95.0 通过：768 项 Rust 通过、67 项条件性 ignored，278 项前端和 80 项维护工具通过；格式、Clippy `-D warnings`、类型检查、前端构建、三 binary debug/release 构建通过。128 篇文档链接/结构检查通过；后续现场记录的文档增量另行检查。

新增固定 origin / 端口 / Cookie 身份 / 绑定容量 / 重绑撤销 / 配置互斥测试。独立 GPT-6 评审发现并关闭两个部署前问题：

- Caddy `{host}` 会丢失路由端口，改为 `{hostport}`，实际 HTTPS 请求已到达正确 origin。
- 固定 UUID 重绑时旧控制连接可能跨越凭据检查与安装的间隙。接入复查在 tunnel 锁内完成；删除身份及移除连接持有一致的 `tunnels → store` 锁顺序。新增四 worker、实际 WebSocket 的确定性回归，暂停旧接入后解绑、换账号绑定并接入新连接，再恢复旧接入；旧连接不能获得 Welcome/grant 或替换新 epoch。18 项 relay library 和 1 项 CLI 测试通过，独立复审未发现这两项仍有阻塞。

## 服务端现场验证

后端仅监听 `127.0.0.1:8787`；新增 Caddy 管理入口仅 `127.0.0.1:2020`，HTTPS 测试入口为 9443 / 9444。systemd 实测 CPUQuota=350ms/s、MemoryLimit=402653184 bytes、TasksMax=64。TLS 容器固定复用已存在的 Caddy 2.11.4 镜像 ID，0.15 CPU、128 MiB 内存 / swap 上限、64 PID、只读根和配置/证书目录挂载、仅 NET_BIND_SERVICE、no-new-privileges、日志最多两份各 5 MiB。

源 IP 证书为 Let's Encrypt YE2，SAN 包含目标 IP，现场有效期 2026-10-11 至 2026-10-17 UTC。两个新 HTTPS origin 在服务器内部经实际 Caddy、保留证书校验测试：门户返回 200，未登录工作台返回 401，`ssl_verify_result=0`。既有 IP HTTPS 443 仍返回 200、证书校验为 0。

首轮 TLS 测试失败：IP 客户端未发送 SNI，且原子替换单文件配置后 Docker 文件挂载仍指向旧 inode。增加 [default_sni](https://caddyserver.com/docs/caddyfile/options#default-sni)并改为配置目录挂载后复验通过；只重建本次新增 TLS 容器，未操作原有容器。只使用 h1/h2，未新增 HTTP/3 UDP 入口。

证书刷新只读取源证书摘要，每 10 分钟检查，仅重载新增容器。初次刷新和模拟测试摘要变化均通过，刷新后 HTTPS 仍为 200 / 校验 0；模拟未修改源证书或私钥。真实后续续签尚未发生，不能将模拟当成续签验收。

部署后与原有快照逐项核验：8 个容器 ID、启动时间及重启次数不变，全部仍运行，有健康检查者仍 healthy；原有 Caddy 配置、镜像、容器和证书续签职责未修改。一次空闲采样为 relay 约 1.3 MiB、TLS 容器 16.25 MiB，服务器可用内存 848 MiB。该采样不是峰值承诺，硬上限与应用有界预算继续生效。

## 公网复验和未验证

初次从本机直连两个新公网端口超时；服务器内部 HTTPS 正常且 INPUT policy 为 ACCEPT。用户确认仅允许 TCP 9443 / 9444 后复验：公网门户 200、未登录工作台 401，两个 origin 的 `ssl_verify_result=0`，未关闭 TLS 验证。8787 / 2020 仍只监听 loopback，没有公开或改变现有防火墙。

16:06–16:09（Asia/Shanghai）执行私有系统 Chrome 探针，390×844 手机视口、隔离 HOME/USERPROFILE/CODEX_HOME、合成 CLI 与独立验证账号，检查全部通过：

- 实际邀请兑换与账号登录，本机 owner 网页配对、绑定确认和显式开启；原生连接器经公网建立受信任 WSS。
- 门户 Open 的一次性交换自动建立真实 Secure/HttpOnly Cookie，进入对应 Application 的 RemoteOwner 首页；3 个网页资源响应、设置 UI 通过；远程目录选择窗口不可用，保留路径启动。
- 合成项目启动、终端 WSS 输入 ACK、SSE 返回 200；恶意 Origin 写入返回 403。
- 忘记绑定后远程访问返回 401，本机合成 CLI 仍存活；随后停止合成运行、清除测试绑定、注销并关闭隔离 Application。

探针聚合结果为 `passed:true`、`stage:complete`；测试账号随后禁用，门户没有遗留测试工作台，进程检查未发现隔离探针目录中的残留进程。验证结束时保留本人的试用邀请、本机试用 Application 和独立配置/历史目录。一次性邀请和测试凭据不进入本报告或 Git。

测试后再次比较原有 8 个容器启动时间及重启次数，全都不变、全部运行、有健康检查的 3 个仍 healthy。新增 relay 无重启，TLS 容器无重启，限额和 loopback 监听范围通过复验；一次空闲采样 relay 约 0.8 MiB、TLS 16.32 MiB，服务器可用内存 884 MiB。未使用真实模型或真实 CLI 会话，不能把合成运行当成完整原生交互验收。

公网基础访问和原生连接器链路可供试用；真实手机、完整 V2 的原生模型/审批/图片、续签跨越、重启/断网/锁屏恢复和备份恢复尚未完成。域名模式的多工作台验收保持独立。阶段仍为 In progress，不能覆盖既有 V2 Passed 结论。

## 本机停止与源码交付

用户随后明确要求停止本地并将当前中转站代码 commit/push。已对本次试用 Application 的准确进程发送 Ctrl-C，进程正常退出，loopback 监听和私有入口消失；配置、绑定偏好和历史保留，云端中转服务继续运行。提交前统一 README/CHANGELOG/指南中的受测范围，并检查暂存区二进制与敏感资料；提交与远端结果以实际 Git 记录为准，不把公网基础链路通过写成全部验收完成。

## 分支隔离复验

用户进一步要求 V3 / Telegram 消息渠道独立维护。`codex/public-relay` 改为基于远端 V2 `main` 的 `b0ae1292d74a4f5a4ce41dda1ce4dc929bda17ef`，仅应用公网中转的源码、测试与文档。V3 原有 21 个提交完整保留在 `codex/v3-channel-origin-spike`，头为 `c6e8e2b`，未改写历史。仅用于拆分保护的 `codex/public-relay-snapshot` 留在本地，不作为发布分支。

移除合并上下文带入的 V3 StateData 字段、渠道目录、路由和 Telegram handler，保留 V2 的 Application/Run/PTY 生命周期，只增加 relay 配置根、独立 ingress 和受理前权限复查。独立 GPT-6 只读复审确认 `src` / `web/src` 不包含 V3 标志、配置、模块、面板或渠道路由，中转站不依赖 V3 实现。上述公网服务器验证使用的是拆分前产物；本次未重新部署云端。停止本机后公网门户再次返回 200、TLS 校验 0。

独立基线的 `node scripts/dev.mjs check --offline` 通过：481 项 Rust、238 项前端、80 项维护工具，56 项条件性 Rust ignored 不计作通过；严格 Clippy、格式、类型及先 Web 后三个 binary 的 debug/release 构建通过。产物为 `target/artifacts/9aa97e5e-1d4e-4ccf-a7f0-9ba66642ff2c`。最初沙箱不允许 loopback 监听，检查被中断；失败日志保留在 `target/artifacts/58726f78-46c4-45c8-8ef0-d551ea379abd`，不计作通过，允许本地网络后完整重跑得到上述结果。

`node scripts/dev.mjs e2e` 使用系统 Chrome，10 项通过，无浏览器下载；产物为 `target/artifacts/dd555109-6c46-4f02-a94a-f338f54ffe3f`。已知分支历史二进制检查覆盖 1,585 个唯一 blob，无禁止项；V3 的 90 个变更文件及 21 个提交补丁另经有限敏感模式检查，未发现凭据或私人会话内容，不将静态模式扫描当作绝对无泄露证明。提交前另复查最终暂存区和文档。

标准检查构建出的 library 测试产物另以 `CODEX_RELAY_CHROME_TEST=1` 显式运行 `public_relay_routes_two_accounts_full_owner_http_terminal_and_logout_without_stopping_cli`，1 项通过，50.50 秒，系统 Chrome 155.0.8059.40。两个合成账号、两个真实中转工作台、390px 手机视口、网页资源、启动/终端/SSE、跨账号拒绝和注销链路通过；CLI 在远程撤销后继续运行。产物为 `target/artifacts/70cd6150-49e2-4997-a0d9-b40979b3e3a9`，独立临时目录与合成 CLI，不恢复已停止的本人试用工作台。该复验仍不能替代真实手机完整功能验收。
