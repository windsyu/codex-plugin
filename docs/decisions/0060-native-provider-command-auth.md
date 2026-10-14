# ADR 0060：命令认证由原生 CLI 持有

- Status: Accepted
- Date: 2026-10-14

## Context

公网门户登录只证明浏览器可访问所属电脑和工作台，不提供模型服务凭证。现有 `unmanaged-custom` 适配器仅接受静态 bearer，并将命令认证及原生登录存储一并拒绝，阻止使用原生 provider `auth.command` 的配置启动。命令凭证的生成、缓存和失效重取属于安装版官方 Codex CLI，不应在工作台重复实现。

本决定是项目接入边界，不据此宣称任意 CLI 版本、provider 或真实模型已验证。实现和分项证据见[验证记录](../validation/native-cli-command-auth-2026-10-14.md)。

## Decision

1. 中转登录与模型认证分离。账号、Cookie、电脑登记和网关授权只保护网页/网关访问；本机官方 CLI 仍是模型认证的唯一执行者，门户不接收或下发模型 token。
2. `unmanaged-custom` 继续要求原生 `custom` provider、`wire_api=responses`、显式上游和显式布尔 `requires_openai_auth`。接受非空 `model_providers.custom.auth.command`，可选 `args` 必须是字符串数组；命令路径和参数按原配置交给 CLI。工作台只校验配置形状、路由与冲突，不执行命令、不读取命令程序/输出、不获取或缓存生成的 token，不记录命令/参数或 token。
3. 命令认证要求 `requires_openai_auth=false`，与 `env_key`、任何已配置的 `experimental_bearer_token`（包括空值或错误类型）、`env_http_headers`、AWS 配置互斥。命名 profile 的 `auth` 子表按原生递归覆盖语义检查。拒绝 malformed auth、歧义及冲突，并只向页面/诊断输出固定错误码。系统/managed 配置来源仍拒绝，不以放开认证为由放开未经验证的有效路由。
4. `auth.json` 或 credential store 不提供命令 token，但原生登录状态可能独立启用 cloud-managed 配置，并为显式 provider 补入未检查的字段。因此命令路径保留既有来源门禁：允许缺失的 `auth.json`，或 File store 中 API-key 形状的文件；拒绝 malformed/ChatGPT token 文件及 `keyring/auto` 等未验证存储。工作台只为有效路由检查读取并快照这些字节、将可见 API key 纳入观察脱敏，不把它当作命令凭证，也不执行命令。spawn 前同时复查配置和该快照；命令输出、缓存、失效和 401 重取仍完全交由 CLI。
5. 固定 upstream 的代理透明转发 CLI 提供的 `Authorization`，不替换、不解析或缓存 bearer；请求/响应头不进入观察副本、调用详情、网页或 journal。观察只取既有安全 metadata 白名单和有界正文。不得将动态 token 加入工作台秘密快照来实现刷新。
6. 静态 bearer 路径保留 ADR 0054 的 `requires_openai_auth=true/false`、File API Key 脱敏及认证快照检查，不改为命令路径，也不扩大到环境认证、managed/ChatGPT provider、AWS 或未经验证的原生 WS profile。

## Alternatives

- 工作台先执行命令获得 token：重复官方凭证逻辑，引入命令执行、秘密缓存和刷新责任，不采用。
- 要求用户改成静态 token 或先做 OpenAI 登录：改变原生配置和凭证生命周期，不采用。
- 无条件开放全部 provider/auth/config 来源：无法证明有效路由与策略边界，不采用。

## Consequences

此次不新增工作台配置格式、数据库 migration、relay schema 或旧 `/v1` 控制能力。不修改原生配置/凭证，不将 model response completed 当作整轮结束。配置和原生登录文件的快照复查仍不是锁，也不保证运行中路由编辑安全。因有效路由适配器尚未覆盖 keyring/cloud-managed 合并，部分由 CLI 自身可运行的命令认证配置会保守拒绝；这不是让工作台接管命令凭证。

动态凭证不被工作台读取，因而不能按其精确值脱敏任意正文中由上游或工具主动回显的 token；认证头排除与既有正文安全策略不等于任意秘密泄漏防护。命令安全、输出格式、凭证有效期与刷新行为仍由用户和 CLI 负责，错误可在原生终端诊断。安装版 CLI/浏览器、真实 provider 和公网发送须各有实际证据，未运行的测试保持待验。
