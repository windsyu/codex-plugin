# ADR 0051：同一工作台的多地址访问与配对

- Status：Accepted（用户要求按计划实现；共用接入已落地，真机结果按实施计划单列）
- Date：2026-09-21

## Context

用户要求二维码/配对链接与本机 IP、Tailscale MagicDNS，并指出初稿将两种 URL 设计成互斥模式、各自会话和转发路径造成不必要复杂度。本次撤回互斥入口与强制 Serve 的选择。随后用户完成手机输入试用，要求配对码保持到程序退出，下次启动才更换，并授权完成该调整。

变更前 Web 固定 loopback、单 Host/Origin，需要有限接入适配；前端相对 API 和按 location 构建的 WS 已支持跟随不同地址。网络连通后业务不应感知 LAN/Tailscale。范围仍是同一使用者访问同一个 Run，模型代理维持 loopback。

## Decision

采用[修订设计](../codex-native-cli-workbench-device-access.md)：

1. 一个 Router、API、WebState、Run 和 CLI，同时提供 IP/MagicDNS URL。二维码地址选择只影响展示，不切换服务、撤销另一地址或重启会话。
2. 默认仅本机；显式开启后增加一个共用 0.0.0.0:port IPv4 listener，两种设备 URL 使用同一端口。保留原 loopback listener 只为保持电脑 URL，不按网络建独立服务。明确 wildcard 接受本机 IPv4 网卡连接、LAN HTTP 不加密，不自动改防火墙。
3. MagicDNS 直接访问同一 HTTP 端口。只读发现本机 Tailscale IP/DNSName；不依赖 Serve、证书、Funnel 或代理配置。未来 HTTPS 增强仍保持同一服务。
4. 所有设备地址共用 Run 级固定随机配对码，可重复扫码，随程序退出失效，下次启动重新生成。设备授权仍按 Run/generation 管理：关闭接入撤销浏览器会话，同一进程再开启保持配对码但不恢复旧 Cookie。有效 Cookie 重复扫码复用授权，不消耗新名额。浏览器 Cookie 按主机自然隔离，换地址可使用同一码再次扫码，服务端无两套会话体系。
5. Host 必须在已知地址集合，Origin 必须与本次 Host 对应，由共享 helper 校验。配对撤销统一处理 WS/SSE、排队输入和重连资格。本机管理资格不通过共享链接授予。
6. 同页面板列地址、固定二维码说明、状态及撤销，去掉倒计时与重新生成操作；JSON 最多新增共用端口偏好，不保存 mode、代理配置、开放状态或凭证。

## Alternatives

- 初版 5 分钟一次性邀请：已实现并验证；按用户实际试用反馈改为本次运行固定码，避免配对时反复回电脑更新二维码。

- 初稿互斥 LAN/Tailscale、专用后端与入口绑定身份：URL 差异不需要这种隔离，已撤回。
- 强制 Serve HTTPS：把证书/转发生命周期误当作名称访问前提，本次不采用。
- 仅替换当前 URL 主机名：现有 listener 与来源检查仍会拒绝，需要有限适配。
- 取消 Host/Origin/配对：网络可达不等于拥有 CLI 操作资格，不采用。
- 每 URL 独立前端/API/CLI：无用户价值，不采用。

## Consequences

删除模式切换、Serve 子进程和端口/路由回收前置工作，业务保持一套实现。改动集中于共用监听、地址集合、统一配对/撤销和二维码。固定码仅保存在内存，由本机专用只读接口取回，不落盘；普通状态与设备接口不公开配对码。断开只撤销当前会话，持有分享码者可再次配对；需要停止所有设备访问时使用关闭开关或退出程序。

LAN HTTP 存在同网监听/篡改风险；Tailscale 地址的 HTTP 页面也不自动成为浏览器安全上下文，需兼容受限 API。不同主机 Cookie/草稿不自动迁移。wildcard 覆盖本机 IPv4 网卡，不能宣称仅某一 Wi-Fi 可达；显式开启和配对仍必需。

实现已加入可选设备监听、共用浏览器授权、二维码面板及 access.port；默认关闭，不修改 Tailscale、防火墙或迁移历史。自动化与 Chrome 证据见[初版接入验证](../validation/native-cli-device-access-2026-09-21.md)及[固定码验证](../validation/native-cli-fixed-pairing-2026-09-21.md)。支持范围与真机结果归[唯一实施计划](../v2-implementation-plan.md#device-access-slices)。
