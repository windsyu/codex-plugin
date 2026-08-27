# ADR 0011: Startup-scoped reusable local token

## Context

五分钟、单次消费的本机 Viewer 配对码只能兑换一次。用户需要在同一次 Observer 运行期间把一个稳定 token 用于多个本机应用，而不希望每次接入都重新生成配对码。

## Decision

`codex-observerd serve` 成功绑定 loopback 并完成可选 Tailscale Serve 前置检查后，生成新的 256-bit bearer secret，并以 `0600` 文件原子替换 `bearer_token_file`。该 secret 在本次 `serve` 生命周期内不再变化；下一次 `serve` 启动时再次轮换。

本机 Viewer 的 fragment 配对 token 由当前启动 secret 确定性签名，同一次启动内保持相同且可重复兑换，直到 daemon 重启。`codex-observerd open` 只读取当前 token 文件并打印相同的配对 URL，不触发轮换。Viewer 仍立即从地址栏删除 fragment，兑换后仍返回 `HttpOnly; SameSite=Strict` Cookie；Bearer 认证继续供其他本机应用和 CLI 使用。

重启轮换会同时令上一次启动的配对 token、Bearer token、签名 Cookie 和分页 cursor 失效。配置的 token 文件继续是其他本机应用获取当前 Bearer token 的唯一持久位置，不把 secret 放入 query、日志或仓库。

## Alternatives

- 保留单次配对码并为每个应用执行 `open`：暴露时间更短，但没有解决多应用复用和操作成本。
- 使用长期、跨重启不变的 token：最方便，但扩大泄露后的有效窗口，不接受。
- 为每个应用维护独立持久 token：可单独撤销，但需要凭证注册、存储和管理界面，超出 V1 单用户 MVP。

## Consequences

同一次启动内，一个 token 可以被多个本机应用重复使用；重启成为明确、容易理解的轮换边界。持有启动 token 的进程在 daemon 重启前可以建立新会话，因此 token 文件仍必须保持 `0600`，Viewer fragment 仍不得进入历史、Referer 或持久 Web storage。

Cookie 的声明有效期仍为 30 天，但实际认证有效期取 Cookie 过期时间与 daemon 本次启动生命周期中的较短者。重启后客户端需要使用新 token 重新配对，进行中的签名分页 cursor 也需要从第一页重新查询。

## Status

Accepted; supersedes ADR 0006 的五分钟单次消费决定。

## Date

2026-08-27
