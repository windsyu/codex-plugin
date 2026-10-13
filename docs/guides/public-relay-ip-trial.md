# 单工作台 IP 测试

本入口落实 [ADR 0057](../decisions/0057-single-workbench-ip-trial.md)，用于在可信现有服务器试用。原有多账号、多工作台正式部署继续使用 [域名部署指南](public-relay-deployment.md)。实际验证结果另见验证索引；示例参数不是已完成现场验收的证明。

## 地址与限制

门户使用 `https://<公网IP>:9443`，工作台使用 `https://<公网IP>:9444`。两个端口需要安全组允许访问；后端 `127.0.0.1:8787` 和新 Caddy 管理端口 `127.0.0.1:2020` 只供本机使用，不能公开。

必须有包含该 IP 地址的受信任证书，不关闭浏览器或连接器的证书校验。每个测试服务只允许绑定一台工作台；再次绑定先在门户忘记当前工作台。同 IP、不同端口的 Cookie 作用域相同，已有 HTTPS 服务也属于凭据信任边界。端口保持独立 Origin 校验，但不能冒充逐主机 Cookie 隔离。

## 隔离部署

在服务器外构建与平台匹配的 `codex-relayd`，再上传并核对 SHA-256；不在资源有限的生产主机安装 Rust 或编译。先记录现有容器 ID、启动时间、重启次数、健康状态、内存余量和端口，拒绝覆盖已有测试路径或占用端口。

使用 [独立服务模板](../../deploy/relay/codex-relayd-ip-trial.service)及 [独立 Caddy 模板](../../deploy/relay/Caddyfile.ip-trial.example)。服务路径为 `/opt/codex-relay-trial/bin/codex-relayd`，数据目录 `/var/lib/codex-relay-trial` 由无登录权限的 `codex-relay-trial` 用户独占。`/etc/codex-relay-trial.env` 只包含三个非秘密的部署身份：

```sh
RELAY_PORTAL_ORIGIN=https://<公网IP>:9443
RELAY_WORKBENCH_ORIGIN=https://<公网IP>:9444
RELAY_GATEWAY_ID=<一次生成后保留的UUID>
```

relay 使用 35% 单核 CPU 上限、384 MiB 内存上限、64 个任务上限和一个密码验证任务；模板兼容受测服务器 systemd 239 / cgroup v1，启动后必须读取实际 cgroup 限额核实。哈希参数不变。

新 TLS 容器命名为 `codex-relay-trial-tls`，使用已存在且固定 ID 的 Caddy 镜像，不升级现有镜像或容器。为它单独设置 0.15 CPU、128 MiB 内存、64 PID、只读根文件系统、`no-new-privileges`、仅 `NET_BIND_SERVICE` capability、16 MiB `/data` 和 `/config` 临时目录；日志最多两份各 5 MiB。只读挂载新配置和已有证书的叶目录，不重新标记或改权限已有证书。现有 Caddy 镜像的二进制带文件 capability，因此全部 drop 后执行会失败；只增加该文件所需能力，不能改为 privileged。

将字面 IP 作为 `RELAY_TRIAL_IP` 传给新 TLS 容器。配置目录只读挂载到 `/etc/caddy`，证书叶目录只读挂载到 `/certs`；使用目录挂载，避免原子替换文件后容器仍读取旧 inode。模板的 `default_sni` 为不发送 SNI 的 IP 客户端选择证书，保留 `{hostport}` 以传递两种精确 origin；只使用 HTTP/1.1、HTTP/2，不新增 UDP/HTTP3 入口。模板停用自动 HTTPS 管理，只加载源证书；现有 Caddy 继续负责续签。源证书为短期 IP 证书时，监测公开证书文件摘要变化，仅对新容器执行：

```sh
docker exec codex-relay-trial-tls caddy reload --config /etc/caddy/Caddyfile --address 127.0.0.1:2020 --force
```

刷新流程应有超时、仅写测试目录且只操作这个命名容器；不能重载现有 `caddy` 容器或改变它的续签配置。

## 账号和试用

管理员使用测试服务的三个身份参数及 `--password-workers 1` 发放邀请：

```sh
sudo -u codex-relay-trial /opt/codex-relay-trial/bin/codex-relayd \
  --data-dir /var/lib/codex-relay-trial \
  --portal-origin https://<公网IP>:9443 \
  --workbench-origin https://<公网IP>:9444 \
  --fixed-gateway-id <同一个UUID> --password-workers 1 \
  invite --username trialuser
```

邀请只能兑换一次，24 小时有效；只交给本人，不放入公开日志。用户设置密码后登录门户，在本机新版工作台首页点“公网接入”，填门户地址和名称，生成绑定码，在门户确认，再回到本机“开启公网接入”。门户显示 Online 后点 Open，即进入完整 V2 页面。不要修改已经启用的原生 CLI 或真实工作台配置来迁就测试。

电脑必须在线，手机关闭页面不停止任务；电脑退出工作台会结束自有 CLI。手机终端输入仍需现有终端控制权。输入确认丢失不自动重发。

## 检查和停止

验证门户和工作台 TLS、账号登录、归属拒绝、跨端口 Origin 拒绝、一次性交换、实际 WSS/HTTP/SSE/终端和本机连接状态。检查新增服务的实际资源限额，逐一对照原容器 ID、启动时间、重启次数和健康状态；通过只代表当时观察，没有保证所有未来负载无影响。

停止时只针对新增服务：

```sh
systemctl disable --now codex-relay-trial-cert-refresh.timer
systemctl disable --now codex-relayd-ip-trial.service
docker stop codex-relay-trial-tls
```

停用不会删除账号库、本机历史、证书或既有容器。测试数据和私有凭据不得提交 Git。正式多工作台接入仍应切回门户与工作台独立域名。
