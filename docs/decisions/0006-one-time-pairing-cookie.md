# ADR 0006: One-time pairing and signed Viewer cookie

## Context

要求用户从私有 token 文件复制 bearer secret 容易误操作，也会让长寿命 secret 暴露在剪贴板和浏览器输入框中。

## Decision

`codex-observerd open` 只打印一个五分钟有效的 URL。配对 code 放在 fragment 中，以当前 bearer secret keyed-sign；daemon 的 `POST /v1/auth/pair` 在严格 Origin 校验后单次消费 nonce，返回 30 天签名 `observer_session` Cookie。Cookie 使用 `HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`。V1 是 loopback HTTP，因此不声明无法可靠工作的 `Secure`。Viewer 兑换后立即从地址栏删除 fragment。

## Alternatives

- 继续手工粘贴 bearer token：无新增协议，但暴露面和使用成本更高。
- 在 URL query 放 token：会进入访问日志、历史记录和 Referer，不接受。
- 本地 HTTPS + Secure Cookie：证书分发复杂度不适合当前单用户 MVP。

## Consequences

Cookie 与 bearer secret 绑定，token 轮换会立即失效。过期或重放 code 被拒绝；Bearer 认证继续供 CLI/API 使用。nonce 集合有界且仅驻留 daemon 内存。

## Status

Accepted

## Date

2026-08-14
