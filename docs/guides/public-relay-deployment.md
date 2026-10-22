# Linux 公网中继部署与运维

本指南对应[中转架构与安全契约](../architecture/relay.md)和[交互与 API](../interactions/relay.md)。仅支持单 HTTPS origin：门户、可信前端和 `/g/{gatewayId}/` 入口共享地址，支持多个账号与电脑。浏览器与电脑按 CLI Run 加密，后端只转发业务密文；运营者仍是可信前端与授权方，不防恶意修改脚本/授权。三端须配套 API/wire 3，无明文降级。受测 Linux/TLS、真实手机及规模限制见[支持范围](support.md)；操作方法不代表任意部署已经验收。

## 构建与服务隔离

只部署服务器时从[组件部署入口](component-deployment.md)选择 `relay`；电脑上的后端按[本机指南](local-workbench-deployment.md)单独安装。中转源码在 `relay/`，生产依赖只有共享协议/静态资源，不链接根 package 的 CLI、PTY 或历史运行模块。`src/workbench/relay` 是电脑端连接器，不要把它当成服务端部署。

中继嵌入与本机工作台同一份前端。构建机准备 Rust 1.95.0、Node 22.23.2 和 C/C++ 工具，从受信任 checkout 根目录按序构建：

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs build --component relay --release
node scripts/dev.mjs check --component relay
./target/release/codex-relayd --help
```

标准入口先构建前端再构建所选 Rust package，不允许旧前端搭配新中继。原生默认 Cargo 配置下，产物为 `target/release/codex-relayd`；配置了 target 或外部 target directory 时按实际 Cargo 输出定位，不从旧缓存拾取同名产物。目标服务器只运行中转，无需安装 Node、Codex CLI 或工作台历史库。此构建应在目标 Linux 构建环境执行；macOS 产物不能直接部署 Linux。`package` 仅生成本机 macOS 安装包，不是服务器发布入口。

分发时须保留项目 `LICENSE`、[第三方声明](../../THIRD_PARTY_NOTICES.md)、实际 Cargo/npm 运行依赖的原始通知与 Codicons 归属；不能因为仅分发 relay 就省略终端和其他嵌入前端依赖许可。`scripts/lib/release.mjs` 提供 `writeLicenses` / `writeChecksums` 共用收集器；独立目标包装须保留来源 commit/dirty、target、锁文件、许可索引及校验和，不包含密码或数据目录。静态链接还必须显式提供实际 sysroot/工具链运行库通知，不把 Cargo/npm 索引当作完整静态运行库许可。当前不提供通用 Linux 归档/自动安装命令，完整边界见[发布指南](../development/releasing.md)。

使用无 OS 登录权限的专用服务用户，数据目录 `0700`、数据库及 sidecar `0600`。中继仅监听 `127.0.0.1:8787`，公网仅暴露已经选定的 HTTPS 入口。不要新增通用代理或公开后端端口。安装前核对架构、路径、已有服务和备份；不覆盖运行中的其他服务。

## 单 origin 启动与 TLS

以门户 `https://app.example.com` 为例（IP 和非默认端口见[下一节](#单-ip-与受限服务器部署)）：

```sh
codex-relayd --data-dir /var/lib/codex-relay \
  --portal-origin https://app.example.com --single-origin \
  --trusted-client-ip-header x-relay-client-ip \
  serve --bind 127.0.0.1:8787 \
  --bootstrap-password-file /var/lib/codex-relay/bootstrap-password
```

首次部署必须通过私有文件提供默认 `windsyu` 管理员的临时密码。使用本机安全编辑器准备上述文件，由服务用户拥有、权限 `0600`，内容只有密码及可选末尾换行；禁止符号链接，不把密码写入命令行、环境变量、日志或仓库。文件父目录同样私有。密码须为 15–128 个字符并通过常见密码检查。首次登录强制改密，完成后重新登录，方可访问工作台与管理页面。初始化完成后安全移除临时密码文件，并从服务启动参数移除 `--bootstrap-password-file`。

[单 origin systemd 模板](../../deploy/relay/codex-relayd-single-origin.service)使用非秘密环境文件设置门户地址；[单 origin Caddy 模板](../../deploy/relay/Caddyfile.single-origin.example)使用已有证书。替换路径并验证配置后才启用。TLS 代理须保留精确 Host（非默认端口必须保留），移除 `Forwarded`/`X-Forwarded-*`，覆盖 `X-Relay-Client-IP` 为连接来源。服务仅在显式启用可信头且直连来源为 loopback 时接受该值；未启用时所有经代理客户端共享 loopback 限流。

已有 Caddy 站点须合并配置，不能直接覆盖现有配置文件。保留原有证书续签机制，只验证和重载本次修改的入口。不要启用请求正文日志，访问日志也不应记录页面 grant 查询参数。生产入口必须使用受信任 TLS，不关闭证书校验。

单 origin 是唯一且默认模式，`--single-origin` 可显式声明；不为账号分配子域名或额外端口。旧路由模式已移除，见[升级边界](#历史模式退出与升级)。

## 单 IP 与受限服务器部署

单 IP 新部署同样使用 `--single-origin`，例如 `https://<公网IP>:9443/` 和 `/g/{gatewayId}/`，不再为每台电脑开放端口。使用包含字面 IP 的受信任证书，保留浏览器和连接器证书校验。只使用已经选定的 HTTPS 入口，不改其他现有端口；后端 `127.0.0.1:8787` 和 Caddy 管理口 `127.0.0.1:2020` 不得公开。Cookie 不按端口隔离，同一 IP 上其他 HTTPS 服务仍在凭据信任边界内，不能混用不可信服务；页面 grant 不能代替父 HttpOnly Cookie。

在服务器外按目标 Linux 平台构建并核对上传产物 SHA-256，不在资源有限的生产主机临时安装编译工具。先记录既有服务/容器身份、启动时间、健康状态、资源余量和端口，不覆盖现有部署路径。使用上节单 origin 服务与 Caddy 模板，服务数据由无 OS 登录权限的专用用户独占。非秘密环境文件 `/etc/codex-relay-single-origin.env` 包含：

```sh
RELAY_PORTAL_ORIGIN=https://<公网IP>:9443
```

TLS 进程环境设置同一个 `RELAY_PUBLIC_ADDRESS=<公网IP>:9443` 和 `RELAY_CERT_NAME=<公网IP>`。若使用容器，以只读目录挂载现有证书叶目录到 `/certs`，配置的 `.crt`/`.key` 须匹配实际文件名；不要修改已有证书权限、标签或续签配置。

模板限制 relay 为 35% 单核 CPU、384 MiB 内存、64 个任务和一个密码验证任务；启动后读取实际 cgroup 限额验证，不降低哈希成本。若独立 Caddy 容器提供入口，复用已有固定镜像版本，设置 0.15 CPU、128 MiB、64 PID、只读根、`no-new-privileges`、仅所需 `NET_BIND_SERVICE`、有界临时目录及日志轮转，不使用 privileged 或升级原容器。资源限额是部署模板，不是所有负载均可用的保证。

该模板 upstream 固定为 `127.0.0.1:8787`，relay 运行在宿主 systemd。Linux 上独立 TLS 容器必须与 relay 共享宿主网络命名空间（例如显式 host network），或使用另行验证的网络设计；默认 bridge 容器内的 `127.0.0.1` 指向容器自己，会返回 502。启用前核对选定端口没有占用，Caddy 管理口仍仅 loopback，并确认没有新增其他公网监听。不要为修复 502 把 relay 绑定到 `0.0.0.0`，也不要改动现有业务容器网络。

首次启动仍按上节私有文件初始化 `windsyu`，改密后移除参数与文件。证书续签后仅重载本次 TLS 入口；容器挂载目录以避免原子替换后仍读旧 inode，限制重载超时，不重载无关容器或修改其续签机制。模板只启用 HTTP/1.1 与 HTTP/2，不新增 UDP/HTTP3 入口。

## 历史模式退出与升级

按 [ADR 0066](../decisions/0066-run-scoped-encrypted-relay-channels.md)，旧域名/双端口参数、交换码入口及专用模板已移除，仅支持上面的单 origin 模板。三端同时升级，旧工作台页从门户重新打开；不能暂时启用明文回退。账号、登记、电脑凭据、schema 3 及历史保留，不需要删库或重新初始化管理员。旧电脑状态字段保持可读，但不提供旧双 origin 的自动部署迁移；原工作台 origin 改变后，电脑的固定 origin 校验会拒绝直接上线，需在本机按新入口显式重新授权，不直接改写状态文件。升级前备份并核对实际服务、入口和数据目录，不恢复旧库复活已撤销的权限。

## 账号与电脑登录

管理员在门户创建普通账号并设置临时密码，安全交给本人。普通用户首次登录必须修改密码。管理页面提供重置临时密码、禁用、启用和删除账号；不再提供邀请注册或公开密码重置链接。删除仅允许普通账号，不能删除自身/管理员；确认后永久删除该账号的中转电脑登记及全部登录，不删除电脑原生 Codex 历史或停止本机 CLI。同名重建是新身份，不继承旧电脑。管理员只管理账号，工作台列表仍只显示自己的电脑。`invite`、`reset`、`disable` 旧 CLI 不再作为账号管理入口。

本机 owner 在“公网接入”填写服务 HTTPS 地址和电脑名称，点击“登录并开启公网接入”。浏览器登录账号并核对电脑名称、一次性码和完整工作台权限；若弹窗被阻止，点击显式验证链接。确认后本机保存登录并自动开启连接。页面加载或普通状态轮询不会自动打开登录页面。关闭接入保留登录；取消登录终止正在进行的登录；退出登录并撤销接入会清理本机许可并尽力撤销服务器登记，失败时按提示到门户撤销。

修改密码、管理员重置密码或禁用账号会撤销该账号所有网页和电脑登录；电脑保留登记但显示需要重新登录。启用普通账号不会恢复此前撤销的授权；`recover-admin` 仅撤销 `windsyu` 管理员自身的全部登录，不影响其他账号。离线、需要重新登录和版本不兼容必须分开显示。只有在线且版本兼容的电脑可打开。门户授予的页面 grant 固定绑定父浏览器会话；旧页面失效后必须回门户重新打开，不会自动获得新授权或重放输入。

忘记管理员密码时，停止服务、确认进程退出，用服务用户从私有 `0600` 文件执行：

```sh
sudo -u codex-relay codex-relayd --data-dir /var/lib/codex-relay \
  recover-admin --password-file /var/lib/codex-relay/recovery-password
```

恢复命令只需数据目录，不需要路由参数。随后安全移除密码文件并重启服务；`windsyu` 再次强制改密，原管理员网页与电脑登录已撤销。恢复不删除其他账号、电脑登记或本机历史。

## 备份、schema 迁移与恢复

升级前停止服务，或采用 SQLite 一致性备份方式，保留整个私有数据目录、版本与校验和。不要复制正在写入的主库而漏掉 WAL。备份保持 `0600` 并加密保存，恢复到私有目录后核对服务用户所有权。当前 schema 3 的迁移在同一事务将旧管理员 `admin` 改为 `windsyu`，保留 UUID、密码、状态、代际、电脑归属和有效登录；重启幂等。先只读检查目标用户名冲突，任何冲突都拒绝迁移，不能覆盖或自动提权。schema 1 原有升级仍保留账号/电脑归属并撤销旧授权，旧电脑需重新登录。见 [ADR 0061](../decisions/0061-account-deletion-admin-rename.md)。

只有相同 schema/契约且完整性检查通过的二进制升级，才可在核实兼容后仅回退二进制/启动配置并保留当前数据库。schema 3 不能交给只支持 schema 2 的旧版；迁移开始后失败须停止专用服务、保全当前库及配置，不自动回退旧版或恢复旧快照。旧快照会丢失后续删除、改密、禁用和撤销，可能复活已删除账号及旧密码。人工恢复前必须核对并保留这些变更；无法核对则保持停服。代际提升仅撤销凭据，不能代替已删除账号和密码状态的核对。

人工核对上述账号状态后，从旧快照恢复仍可能复活已撤销凭据。停服恢复后，对 schema 2/3 进行全量撤销时保留电脑登记，以服务用户执行如下事务：

```sh
sudo -u codex-relay sqlite3 /var/lib/codex-relay/relay.sqlite3 <<'SQL'
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
DELETE FROM open_codes;
DELETE FROM sessions;
DELETE FROM tokens;
UPDATE accounts SET auth_generation=auth_generation+1;
COMMIT;
PRAGMA integrity_check;
PRAGMA foreign_key_check;
SQL
```

完整性检查必须返回 `ok`，外键检查无结果，才可启动。不要删除 `gateways`；旧凭据因账号授权代际变化而失效，用户重新登录电脑后继续使用登记。备份疑似泄露时还需重置账号密码。服务器不应保存工作台正文，恢复不导入本机历史。

## 验收与停止

电脑通过“登录并开启公网接入”确认并自动连接，手机登录同一门户并选择自己的在线电脑。至少同账号两台电脑、异账号一台，验证多电脑并行、首次改密、管理员操作、固定 grant、退出/改密后旧页拒绝、SSE/终端撤销、慢流/断网不重放、跨账号和跨 origin 拒绝、版本不兼容，以及本机 CLI 继续运行。逐项比较原服务健康与资源状态，实测只代表当时观察。Linux、证书续签、真实手机与停机恢复的实际结果记录到对应 PR 或私有受管产物，持续限制同步到支持文档；桌面手机视口不能替代真实网络验收。

停止时只停止本次新建的 relay 服务、证书刷新任务和专用 TLS 容器，不停止已有业务入口；共享入口部署只撤下本次新增路由。停用不删除账号库、本机历史或证书，私有数据不进入 Git。任何 schema 3 回退/恢复仍遵守上节规则，不自动恢复旧库复活权限。
