# 单端口中转测试部署（2026-10-14）

## 范围与来源

用户授权升级指定 ECS 上已有的中转测试部署。源码为 `codex/public-relay` 的 `fa094c23b3742a6bb6cb475c7ac924fd7357582f` 加本地未提交改动，不是已发布 tag。本次只部署 Linux 中转，不启动本人的电脑工作台、真实 CLI 或模型，不创建 Git 提交或执行 push。

服务器为 Alibaba Cloud Linux 3、x86-64、2 vCPU、约 1.87 GiB RAM、无 swap、systemd 239 / cgroup v1。升级前约 824 MiB 可用内存、28 GiB 可用磁盘。复用原专用服务 `codex-relayd-ip-trial.service`、用户 `codex-relay-trial`、数据目录 `/var/lib/codex-relay-trial`、专用 TLS 容器 `codex-relay-trial-tls` 和现有 IP 证书；没有调整安全组、修改原业务入口、升级镜像或在服务器安装编译工具。

需求契约见 [ADR 0058](../decisions/0058-admin-accounts-single-origin-relay.md)、[中转契约](../architecture/relay.md)。本地自动化与三电脑联调见 [2026-10-13 验证](public-relay-single-origin-2026-10-13.md)，阶段状态仍只在 [V2 实施计划](../v2-implementation-plan.md) 维护。

## 构建与安装

在 Mac 上使用 Rust 1.95.0、Node 22.23.2、固定 Zig 0.14.1 交叉构建。Zig 下载包按[官方索引](https://ziglang.org/download/index.json)的 SHA-256 校验；先生成 Web，再执行 Cargo `--locked --offline --release -p codex-relayd --bin codex-relayd --target x86_64-unknown-linux-musl`，使用独立受管构建目录。

构建包装器修正了 cc-rs 的 Rust target 到 Zig target 翻译，以及重复 musl CRT 链接问题；对应两次失败诊断保留，不计作成功。最终 ELF 为 x86-64 静态产物，无 PT_INTERP / PT_DYNAMIC。首轮随包收集了 Linux 解析依赖与 Web 运行依赖 288 项的许可、Codicons 归属和锁文件。后续独立审查确认此索引不足以覆盖静态工具链运行库；[增量升级报告](public-relay-routes-recovery-2026-10-14.md)记录四类运行库通知补齐及 292 项最终包，不能将首轮索引描述为完整静态分发许可核验。

成功构建记录：`target/artifacts/3748e522-6a48-45a5-8173-dd07707227a0`；部署工具与浏览器探针：`target/artifacts/eebe25e4-f881-4404-aa4d-0c18e4f8b2b1`。最终上传包已剥离 macOS 扩展元数据，上传后核对包及全部分发文件 SHA-256。

```text
binary SHA-256: 67257d125c8233e30c554d02e82747b1717382c412c77aaae1792619ba1d6918
archive SHA-256: cb1a949412009a7e1f795afadcd237de9439a9417dbb915a1d0cb9ff78b2d2ac
```

新产物和许可安装在 `/opt/codex-relay-trial/releases/6d90b52a-f4e3-4c02-8afe-82feb58f1c9e/relay-upgrade`。原二进制保留；systemd 通过专用 drop-in 切换到新产物和 `--single-origin`。保留一个密码 worker、CPUQuota 35%、MemoryLimit 384 MiB、TasksMax 64。后端仍仅监听 `127.0.0.1:8787`，专用 Caddy 管理入口仍仅 `127.0.0.1:2020`。

专用 Caddy 配置验证后原子替换并 reload，复用原镜像、容器及只读证书挂载，只保留已开放的 HTTPS 9443。9444 不再监听；原 443 业务入口和证书续签职责未变。新工作台使用同一 9443 下的 `/g/{gatewayId}/`，不是新增端口池。

## 迁移、恢复与失败记录

停服并确认进程退出后备份原数据库及 WAL/SHM、旧二进制、service、环境配置和专用 Caddy 配置；另用 SQLite `.backup` 生成一致性单文件快照用于比较，不能对仍依赖 WAL 的主库使用 `immutable=1` 并忽略日志。

两次现场校验误报均保留证据并恢复了旧库及配置：第一次就绪探针漏带已配置的可信代理头，直连返回 400；补齐 loopback 可信头后返回 200。第二次对比忽略旧库 WAL，误报 `old.accounts` 不存在；改用一致性快照后，旧账号 UUID、用户名和密码哈希差异均为 0。第二次恢复后实际验证旧服务和公网 TLS 为 200；这不是故障注入或自动化模拟。

最终升级完成 schema 1 → 2，保留原 2 个账号；旧用户为普通角色。现场原电脑登记为 0，因此本次现场归属对比不代表已在公网验证多电脑恢复。新建默认 `admin` 后共 3 个账号。完整性检查返回 `ok`，外键检查无结果。正常重启前后 admin 的 UUID、密码哈希、认证代际和强制改密状态摘要一致，没有重新设置密码。

最新私有备份为 `/opt/codex-relay-trial/backups/6d90b52a-f4e3-4c02-8afe-82feb58f1c9e-retry2`，其加密归档为 `schema1-backup.tar.gz.enc`；独立密钥在 root 私有的 `/etc/codex-relay-trial-backup-keys`。原始回滚快照及失败后的新库也留在 root `0700` 备份目录中，未删除或传离服务器。加密归档已解密到独立 `restore-check`，旧二进制与一致性数据库逐字节相同，schema 1 完整性/外键检查通过；没有将该恢复副本覆盖在线库。

## 登录、公网与资源验证

- 默认 admin 的随机临时密码只在服务器的 `/var/lib/codex-relay-trial/bootstrap-password`，服务用户所有、`0600`；没有进入工具输出、Git、命令行或外部文件传输。首次登录必须改密。初始化参数已从实际服务启动命令移除，文件留给用户首次登录后安全清理。
- 经实际公网 HTTPS 完成 admin 登录、读取角色和强制改密状态、验证 Secure/HttpOnly Cookie。完成改密前电脑列表、管理列表与授权批准均拒绝；退出后重放原 Cookie 仍拒绝。没有修改 admin 密码以替代用户首次改密。
- 本机 curl 对门户及中央登录脚本返回 200，证书校验结果为 0；服务器经实际 TLS 复验相同。恶意 Origin 登录为 403；无固定授权的工作台 API 为 400（缺少授权选择器），不能访问数据。
- 系统 Chrome 154.0.8037.100，390×844 手机视口、独立临时 HOME/USERPROFILE/CODEX_HOME：公网门户、登录表单、匿名权限拒绝通过，页面错误为 0，TLS 校验未关闭。初次强制直连并等待 network-idle 的探针超时；恢复系统默认连接配置并以 DOMContentLoaded 判断文档就绪后通过。该浏览器结果不证明真实手机网络或直连路径。
- 原 8 个业务容器的 ID、启动时间与重启次数逐项不变；全部运行，有健康检查的 3 个仍 healthy。专用 TLS 容器仅 reload，ID、启动时间和重启次数也未变；原 443 返回 200、TLS 校验为 0，既有证书刷新 timer 仍 active。
- 新 relay active/running、NRestarts=0，实际限额保持 350ms/s CPU、402653184 bytes 内存和 64 个任务。一轮空闲 MemoryCurrent 约 1.03 MiB，不代表负载峰值或容量承诺。

## 交付与剩余门槛

服务器已可进行 admin 首次改密、创建用户和配套新版电脑端试用。电脑端必须使用本分支新版产物并重新登录；旧协议/凭据不能继续接入。账号密码、电脑凭据、加密备份及密钥都不纳入仓库。

仍待真实手机网络与输入、至少三台电脑经公网的完整 API/SSE/终端及断网恢复、真实 CLI/模型/审批/图片、实际证书续签跨越验收。前述本地三电脑合成测试、手机视口和本次服务器登录检查不能替代这些结果。不宣称 Linux 工作台支持或完成整个 V2 公网验收。
