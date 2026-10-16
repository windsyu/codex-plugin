# ADR 0056：可信公网中转与账号归属路由

Status: Accepted（实现与公网验收分别记录）

Date: 2026-10-10

## Context

用户明确要求手机经公网中转使用自己工作台的完整 V2 功能，采用邀请制站内密码账号；每个工作台只归一个账号，不引入项目共享、角色或逐设备授权。此前设备接入不包含公网服务，新增阶段不改变已有 V2 验收，也不复用 V3 原生协议实验中的私有 socket relay。

## Decision

- 本机 Application 集成默认关闭的出站连接器；公网服务只管理账号、工作台归属、会话和流转发，不运行 Codex、不代理模型请求、不持久保存会话正文。
- accountId、持久 gatewayId、Application instanceId、runId/runEpoch、connectionEpoch 分开；旧实例、旧连接与已撤销会话不可写入当前 CLI。一个账号拥有工作台所有 V2 业务权限。
- 建立独立 RemoteOwner ingress；不伪造本机 owner Cookie，不信任浏览器提供的身份或转发头，不公开模型代理、私有进程握手、V3 实验或任意 TCP/URL 转发。
- 账号门户和每工作台采用独立 HTTPS origin、host-only Secure HttpOnly Cookie及一次性会话交换。工作台资源由本机同次构建提供。
- 出站 WSS 控制连接承载路由、10 秒续租和撤销；每逻辑 HTTP/SSE/终端流另建有界 WSS 数据连接，避免大输出阻塞控制或其他流。许可 30 秒未续租即失效。没有离线输入队列或 mutation 自动重试。
- 邀请制密码账号使用 Argon2id、随机盐、有界验证并发与账号/IP 限流；运维 CLI 发放邀请、重置和禁用账号。账号与工作台独立凭据均可撤销。
- 公网服务是独立 Linux 部署单元，使用 Rust/Axum、SQLite 与 HTTPS 入口。新增独立 Cargo package 和共享协议 package 组成 workspace，现有工作台 package 仍保留 library 加两个 binary；理由是平台与部署隔离，不重构现有模块边界。

## Alternatives

Tailscale/第三方穿透可保留已有使用方式，但不满足无附加手机网络配置的入口需求。通用反向代理会扩大端口和认证边界。逐项目共享会引入用户不需要的 ACL。端到端加密需要另行解决可信客户端和密钥恢复，首版用户已选择信任服务器。单数据通道复用需要额外流控，独立有界数据连接先验证最小可靠闭环。

## Consequences

中转服务器可读取和控制经它传输的工作台内容；账号路由不提供 OS 沙箱。本机必须在线，断网不停止 CLI。相同 OS 用户下不同账号如需避免历史混合，必须独立配置工作台根、CODEX_HOME 和来源。部署需要门户 DNS、工作台子域与证书。真实公网和手机验收必须另列证据，不能以合成/桌面测试代替。

详细契约和阶段记录已加入文档总索引；旧 `/v1` 保持只读，旧原生历史不做 migration。
