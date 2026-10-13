# Linux 公网中继部署与运维

本指南是 [公网中继契约](../codex-public-workbench-relay.md)的部署草案。命令与文件名须以构建出的 `codex-relayd --help` 和 Linux 实测为准；在 [V2 实施计划](../v2-implementation-plan.md)标记通过之前，不应将示例视为已验证生产安装步骤。示例域名属于保留的 `example.com`，部署时统一替换。服务运营者可信，可看到通过中继传输的工作台内容。

## 准备域名、TLS 和服务用户

用户明确选择直接 IP 的临时单工作台试用时，使用独立的 [IP 测试指南](public-relay-ip-trial.md)。域名模式继续遵循本页，不能把 IP 填入 `--workbench-domain` 来替代子域路由。

将 `app.example.com` 解析到服务器，并为 `*.wb.example.com` 配置 wildcard DNS。门户与工作台必须分别使用 HTTPS origin。获得覆盖门户和 wildcard 的证书，放在只有 TLS 代理可读的私有目录。若使用 Caddy 自动签发 wildcard 证书，须安装适配所用 DNS 提供商的 DNS-01 插件、配置最小权限的 DNS 凭据，并验证证书更新；标准 Caddy 安装本身不能自动完成此步骤。也可以使用外部签发证书并按更新流程替换。

创建无登录权限的 `codex-relay` 系统用户和 `0700` 数据目录，例如 `/var/lib/codex-relay`。数据库、工作台凭据摘要、邀请/重置材料、日志与备份只允许服务用户或指定备份进程读取。不要把真实密码、Cookie 或密钥写入仓库、shell 历史、公开日志或 systemd 单元的命令行参数。只向外开放 TLS 入口；中继进程绑定 `127.0.0.1:8787`。

中转服务独立构建，无需在 Linux 服务器安装 Codex、工作台或前端工具。准备 Rust 1.95.0、C/C++ 构建工具和受信任的版本 checkout，在该版本仓库根运行：

```sh
cargo build --locked --release -p codex-relayd
./target/release/codex-relayd --help
```

构建结果为 `target/release/codex-relayd`；首次依赖获取需要网络。macOS 原生构建及 Linux x86-64 musl 交叉产物的 systemd/Caddy IP 测试启动已有[验证记录](../validation/public-relay-ip-trial-2026-10-13.md)，本页的目标服务器原生构建、域名/wildcard TLS 部署仍须分别验收。本机工作台通过标准 `node scripts/dev.mjs build` 构建，不能用旧后端搭配另一版本前端。

在采用 systemd 的 Linux 主机上，可以由管理员建立服务用户和私有目录；二进制应从本项目受信任的构建产物安装，而不是从示例文档下载：

```sh
sudo useradd --system --home-dir /var/lib/codex-relay --shell /usr/sbin/nologin codex-relay
sudo install -d -o codex-relay -g codex-relay -m 0700 /var/lib/codex-relay
sudo install -o root -g root -m 0755 ./target/release/codex-relayd /usr/local/bin/codex-relayd
sudo install -o root -g root -m 0644 deploy/relay/codex-relayd.service /etc/systemd/system/codex-relayd.service
sudo systemctl daemon-reload
sudo systemctl enable --now codex-relayd.service
```

不同发行版的服务用户命令可能不同；执行前核对目标路径、已存在用户和二进制架构。启动后检查 `systemctl status codex-relayd` 与仅本机监听的 `127.0.0.1:8787`，再启用 TLS 入口和对外 DNS。

预期服务命令：

```sh
codex-relayd --data-dir /var/lib/codex-relay --portal-origin https://app.example.com --workbench-domain wb.example.com --trusted-client-ip-header x-relay-client-ip serve --bind 127.0.0.1:8787
```

仓库的 [systemd 示例](../../deploy/relay/codex-relayd.service)和 [Caddy 示例](../../deploy/relay/Caddyfile.example)仅提供文本模板；安装时核对二进制路径、服务用户、证书权限和重启策略。Caddy 示例使用外部证书路径；如果改用 DNS-01 插件，需由部署者按提供商说明配置并验证通配证书。TLS 代理必须保留真实 Host，移除 `Forwarded`/`X-Forwarded-*`，并覆盖 `X-Relay-Client-IP` 为连接来源地址；服务只有显式启用该选项且上游连接来自 loopback 时才接受这个单一 IP 值。未启用时按直连 IP 限流，Caddy 后的客户端会共享 loopback 限额。限制代理目标为本机端口，设置合理的长连接超时，并避免正文访问日志。不要将 `127.0.0.1:8787` 对外发布。

按 [Caddy 官方说明](https://caddyserver.com/docs/install)安装对应发行版的软件包，替换示例域名/证书并确认运行 Caddy 的用户可以读取私钥后，再安装配置并验证：

```sh
sudo install -o root -g root -m 0644 deploy/relay/Caddyfile.example /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl enable --now caddy
sudo systemctl reload caddy
```

确认门户 HTTPS 可访问、证书受信任，工作台子域解析正确；证书更新和 Caddy 重载需另行演练。现有 Caddy 站点应先合并配置，不能直接覆盖已有配置文件。

## 账号与工作台

当前服务器 CLI 的账号运维命令如下；部署后仍须通过 `--help` 核对实际安装版本：

| 操作 | 预期效果 |
| --- | --- |
| `invite --username exampleuser` | 发放 24 小时一次性邀请；在安全渠道交给本人 |
| `reset --username exampleuser` | 发放 15 分钟一次性重置；用户兑换时撤销旧登录会话 |
| `disable --username exampleuser` | 禁用账号并撤销会话与所属工作台连接 |

单独撤销某工作台时，账号持有人在门户执行删除该工作台的操作，对应 `DELETE /relay/v1/gateways/{id}`；这会撤销服务器侧凭据，不删除本机 Codex 数据。服务器 CLI 当前没有工作台撤销子命令。

账号命令与 `serve` 共用 `--data-dir`、`--portal-origin`、`--workbench-domain` 全局参数。例如以下命令只会把一次性邀请链接打印到管理员终端，应避免终端会话记录外泄，并将用户名与链接一同安全交给本人：

```sh
sudo -u codex-relay codex-relayd --data-dir /var/lib/codex-relay --portal-origin https://app.example.com --workbench-domain wb.example.com invite --username exampleuser
```

用户名使用小写 ASCII；密码长度 15–128 字符。用户在门户注册/登录后，于本机 owner 首页点“公网接入”，输入门户 HTTPS 地址和工作台名称，按一次性码完成绑定，再明确开启。手机从门户进入该账号自己的工作台。每个工作台只归一个账号，账号拥有其完整 V2 权限；没有项目共享或只读角色。关闭保留绑定以供再次开启；忘记会清除本机状态并尝试撤销服务器登记。仅在显示“服务器撤销未确认”时，还需从门户删除工作台。禁用账号应立即阻止新的 HTTP、SSE 和终端会话；已有会话的实测撤销延迟以验收报告为准。

## 备份、恢复与升级

停止服务或使用 SQLite 一致性在线备份方式，备份整个私有数据目录；备份文件权限设为 `0600`，加密后存放在独立位置。不要仅复制正在写入的主数据库文件而遗漏 WAL。一个可审核的停机备份流程是：停止 `codex-relayd.service`，确认进程退出，将 `/var/lib/codex-relay` 完整复制到仅备份管理员可读的私有目录，检查复制结果并加密转存，最后重启服务。记录备份时间、二进制版本和数据库校验和，但不记录账号凭据。恢复时先停止服务，将备份恢复到新的私有目录，核对所有权、迁移版本和门户/工作台域名，再启动并用测试账号验证登录、工作台归属和撤销。恢复后的已撤销凭据可能因旧快照复活；恢复流程必须比对撤销记录，无法确认时按下面步骤撤销所有会话、未兑换邀请/重置码和工作台凭据，用户重新登录并绑定。首版没有可轮换的统一服务密钥，不能用更换不存在的密钥代替撤销。不得从备份导入工作台正文，因为服务器不应保存正文。

停机恢复后的全量撤销针对 schema 1 执行，保留账号密码摘要；运维人员先保存恢复副本，然后用服务用户运行 SQLite 客户端。此操作会注销所有用户并删除所有工作台登记，应作为恢复流程的一部分明确告知用户：

```sh
sudo -u codex-relay sqlite3 /var/lib/codex-relay/relay.sqlite3 <<'SQL'
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
DELETE FROM open_codes;
DELETE FROM sessions;
DELETE FROM tokens;
DELETE FROM gateways;
COMMIT;
PRAGMA integrity_check;
PRAGMA foreign_key_check;
SQL
```

要求完整性检查返回 `ok`，外键检查没有结果，再启动服务；过期的邀请需重新生成。若备份可能泄漏，账号密码也须通过管理员重置流程逐个重置。不要在服务运行时直接执行恢复或改库。

升级前保存上述一致性备份、记录二进制版本与数据库 schema，执行与版本匹配的迁移，再用受邀测试账号检查登录、绑定、V2 阅读/输入、禁用和重连。升级失败时停止服务，恢复匹配版本的二进制及数据库备份；不要只回滚其中之一。升级与恢复期间，本机 CLI 可继续运行，但公网输入中断且不会自动重放。正式 Linux、TLS、手机和恢复演练结果应记录在 [验证索引](../validation/README.md)，不能用本指南代替。
