# ADR 0010: Tailscale Serve loopback forwarding

## Context

本机 Viewer 使用五分钟单次配对链接。跨设备访问时反复生成链接不方便，而直接监听 LAN 会扩大暴露面并要求 Observer 自行管理 TLS。实机验证确认 Tailscale Serve 能将 tailnet HTTPS 转发到 `127.0.0.1`，自动管理证书、应用 Tailnet ACL，并删除客户端伪造的 `Tailscale-User-*` 后注入真实登录身份。

## Decision

Observer 继续只监听 loopback。配置显式开启 `server.tailscale_serve` 后，`serve` 启动时幂等建立 `Tailscale HTTPS → Observer loopback HTTP` 根路径代理，并打印稳定 MagicDNS URL。远程请求仅在 TCP 对端为 loopback、Host 匹配本机 MagicDNS authority、`X-Forwarded-Proto=https` 且存在 Serve 注入的 `Tailscale-User-Login` 时复用现有只读 API；本机 bearer、配对 Cookie 和 Viewer 流程保持不变。

Observer 不创建第二套用户、长期链接 secret、远程 session 或原生 TLS 服务。配置冲突时拒绝覆盖现有 Serve 端口；不调用 Funnel。

## Alternatives

- Observer 直接绑定 Tailscale/LAN IP 并加载 PEM：需要证书续期和额外网络安全面，放弃。
- 长期 Viewer token：仍需分发、保存和轮换 secret，未利用 Tailscale 已验证身份，放弃。
- 独立远程控制面和用户体系：超出只读 V1 MVP，放弃。

## Consequences

Tailnet 内使用固定 HTTPS URL，数据仍由原 Viewer/API/SSE 路径提供。Tailscale CLI、MagicDNS 和 HTTPS capability 成为该可选启动模式的前置条件；失败时启动明确报错。TLS 证书域名会进入 Certificate Transparency，机器名不得包含敏感信息。相同机器上的进程仍位于当前单用户信任边界内。

## Status

Accepted

## Date

2026-08-27
