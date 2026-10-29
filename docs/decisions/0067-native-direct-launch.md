# ADR 0067：原生直连启动与可选模型捕获

## Status

Accepted。用户明确选择先交付原生直连兼容：保留终端和历史，明确提示模型实时捕获不可用。

## Date

2026-10-29

## Context

v1.0.0 将所有启动交给 custom 捕获适配器，要求显式 `model_provider="custom"`、`wire_api` 和 `requires_openai_auth`。原生合法的省略 provider 配置因此被拒绝，又被错误提示为读取权限或 TOML 语法问题。用户要求非必要原生配置不被工作台重复校验，并保留原生默认 OpenAI 路由。

公开 [配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)说明 provider 默认是 `openai`，自定义 provider 的 wire API 默认为 `responses`、`requires_openai_auth` 默认 false。源码研究固定在官方 CLI 0.165.0 的 commit `360319d8dee62d87e2d59fc0c21805d79519cfed`：

- [内建 provider](https://github.com/openai/codex/blob/360319d8dee62d87e2d59fc0c21805d79519cfed/codex-rs/model-provider-info/src/lib.rs)保留原生认证与 WebSocket 能力。
- [TUI](https://github.com/openai/codex/blob/360319d8dee62d87e2d59fc0c21805d79519cfed/codex-rs/tui/src/lib.rs)使用 embedded app-server；[账户路由](https://github.com/openai/codex/blob/360319d8dee62d87e2d59fc0c21805d79519cfed/codex-rs/app-server/src/request_processors/account_processor/workspace_routing.rs)对 ChatGPT 账户发现 backend origin 与 routing override，没有个人订阅豁免。
- 将 `openai_base_url` 换为 loopback 后，原生将其视为独立目的地，不能证明仍保持动态路由语义。因此不能凭个人 JWT plan 判定固定 ChatGPT upstream 安全等价，也不能以禁用 WS、改 provider 或修改 bootstrap URL 作为绕过。

这些是固定源码事实和兼容选择，不等同于真实模型端到端验收。

## Decision

1. 将普通 CLI 启动与模型捕获拆开。默认 OpenAI、其他原生 provider，以及工作台无法安全探测的配置走原生直连。只进行启动自身的目录、binary、进程和本机资源检查；配置和认证由 CLI 处理。直连不读取 AuthSnapshot、不校验 JWT/credential store，不覆写 provider/base URL/auth/transport，也不建模型代理或 Observer。
2. 显式选择 custom 才启用原有捕获适配器。保留固定上游、来源、认证和传输的必要安全门槛；省略 wire API/认证布尔值采用已验证的原生默认。未知、无关、未选 provider 配置不作为通用启动门槛。捕获要求未通过用精确的脱敏诊断，不能误称 TOML 不可读取或解析。
3. 直连明确表示 `capture=unavailable`，发布 `model_capture_unavailable` run 级诊断，使用既有 notice、checkpoint 和 metadata gap reason 持久化。历史覆盖 partial，保存成功不意味着模型捕获完整；用量未知不显示为零次真实模型调用。旧格式无需 migration，已检查的 v1.0.0 Preact 前端仍能显示 notice 说明并视为不完整；不承诺所有历史 reader 都理解新状态，正式部署使用配套前端。
4. 原生终端、生命周期、文件阅读和首页原生历史继续可用。直连不声称模型正文、调用详情、用量或中间态已被工作台记录。

## Alternatives

- 让用户添加 custom：会改变用户选择的原生路由，拒绝。
- 固定 ChatGPT endpoint 加 JWT 门禁：无法保留原生动态账户路由，拒绝。
- 全机 TLS 劫持、安装 CA、修改官方 CLI：不符合当前安全与原版 CLI 边界，拒绝。
- 继续阻止所有默认配置：不满足可启动要求，拒绝。

## Consequences

用户无需修改默认原生配置即可进入终端，工作台不再以捕获限制阻止原生直连。代价是这一路径没有模型实时观察；原生历史是完整会话阅读的来源，工作台运行记录必须明确不完整。完整 OpenAI/ChatGPT 动态路由捕获留作独立设计，不在本次兼容修复中承诺。直连配置错误仍可能由 CLI 拒绝，工作台启动成功不代表原生认证或模型请求成功。

验证覆盖无路由参数覆盖、无认证读取/代理创建、未选 custom 隔离、默认值、未知字段、捕获状态持久化和既有 custom 负向场景；另以实际 CLI 0.165.0 和隔离临时 HOME/USERPROFILE/CODEX_HOME 通过原生默认 OpenAI 的合成 WebSocket 专项：原生配置自带 loopback endpoint/API key，终端收到回复、实际 rollout 可从历史库读取、工作台模型捕获保持不可用且不创建代理。该专项不等于真实 ChatGPT、云端动态路由或全版本综合兼容验证；真实模型请求不是本次验收前提。
