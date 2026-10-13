# ADR 0057：独立端口的单工作台 IP 测试

Status: Accepted（仅测试接入；验收另列）

Date: 2026-10-13

## Context

用户要求在已有重要容器、资源有限的公有云服务器测试中转站，明确选择直接使用 IP，并授权复用现有 Caddy 的 SSL 证书。ADR 0056 的 `g-<gatewayId>.<domain>` 路由无法通过裸 IP 使用。服务器已有受信任的 IP 证书，不能覆盖或重启现有站点。

## Decision

- 保留默认域名路由，增加显式 `--workbench-origin` 与 `--fixed-gateway-id` 测试模式。门户和工作台必须为同一字面 IP 的 HTTPS 地址，使用不同的显式非默认端口；两种模式互斥。
- 测试服务全局只允许一个已绑定工作台。固定 UUID 随部署配置保存，未知 UUID 拒绝路由；启动拒绝不兼容的账号数据库。绑定使用事务拒绝第二台；解绑后可重新绑定，旧凭据和会话继续失效。
- 原有账号归属、Origin/CSRF、一次性会话交换、RemoteOwner、实例/连接代际、租约和撤销保持不变。协议与 SQLite schema 不变。
- 不同端口提供独立 JavaScript origin，但不提供 Cookie 主机隔离。该 IP 上的所有 HTTPS 服务均在测试凭据的信任边界内；本选择不得宣称继承了 ADR 0056 的逐工作台主机 Cookie 隔离。
- 在服务器外构建 Linux 二进制；服务使用独立用户、数据目录、loopback 端口和资源上限。TLS 由独立 Caddy 实例提供，仅只读挂载已有 IP 证书目录；不得改动现有 Caddy 配置或证书续签职责。
- `--password-workers` 默认 4，可降低到 1–4；测试设为 1，Argon2id 内存和工作参数不降低。额外 TLS 实例需在源证书更新后仅重载自身配置。

## Alternatives

门户与通配工作台子域提供原有主机隔离，仍是正式多工作台部署方式。额外公网 IP 需要额外资源。HTTP 或关闭 TLS 校验会破坏已有认证前提。使用第三方 DNS 服务不符合本次直接 IP 的选择。

## Consequences

这是单工作台功能试用入口，不能替代多工作台域名模式验收。同 IP 的其他 HTTPS 服务可能收到中转 Cookie；只应在这些服务均可信的测试服务器使用。证书复用不自动赋予新 Caddy 实例续签能力，必须验证源证书更新与自身重载。停止测试仅停止新增服务和容器，不操作既有应用。

操作与限制见 [IP 测试指南](../guides/public-relay-ip-trial.md)。
