# ADR 0054：保留显式 provider bearer 与原生 API Key 认证

- Status：Accepted
- Date：2026-09-23

## Context

R6-E 从首页选择新项目后，启动器把 `requires_openai_auth=true` 一律当作未验证的 OpenAI 登录而拒绝，网页又将不同错误统一显示为“Codex 未能启动”。目录核验通过仍不能进入 V2；问题来自适配器的认证假设。

只读参考官方仓库 commit `633ab199cfd724aa78013c006b27a2b3d049fc3b`：`model-provider/src/auth.rs` 的 `resolve_provider_auth` 优先使用显式 provider bearer，再考虑原生认证；同文件的解析顺序为 env key、静态 bearer、原生 auth。配置 schema 对 `requires_openai_auth` 的说明包含 API Key 与 ChatGPT。`core/tests/suite/external_auth.rs` 已覆盖 bearer 优先于原生认证，但该官方测试本身不能证明 true 的安装版行为。实际安装版 0.155.1 用独立本机合成服务验证，见[验证记录](../validation/native-cli-r6-e-native-auth-launch-2026-09-23.md)。

## Decision

1. `unmanaged-custom` 接受显式布尔 `requires_openai_auth=true/false`；仍要求 `custom`、Responses、明确上游与非空静态 `experimental_bearer_token`。不改写该值、不代替原生 CLI 选择或注入认证。
2. 保持已有来源限制：拒绝 env key/header、命令认证、AWS、系统/管理配置、keyring/auto、ChatGPT tokens 或可能云管理，以及未验证的显式 WS profile。此修正不等于支持全部 API Key 或 ChatGPT 接入。
3. 从受限读取的 `auth.json` 获取 API Key，仅用于观察副本脱敏；认证快照不实现 Debug/Serialize，不进入 API、日志或记录。保留文件缺失状态并在 spawn 前检查字节是否变化；变化即取消本次启动。
4. 错误分类从 runtime 传递至 Application 和前端；API 与日志只输出固定码，首页和操作面板复用中文说明，不输出底层错误链。

## Alternatives

- 要求用户将该原生标志改为 false：掩盖错误假设并修改原本合法的配置，不采用。
- 无条件开放 OpenAI 登录和所有认证来源：缺少管理策略、token 刷新与路由验证，不采用。
- 启动器读取 API Key 后自行构造上游 Authorization：改变凭证选择与代理职责，不采用。

## Consequences

兼容已有 true + 显式 bearer + File API Key 的配置，目录选择不再被错误阻止。新增认证材料脱敏及启动前变更检查，复杂度局限于现有适配器，不新增配置字段、数据库迁移或持久控制内核。

快照检查不是文件锁，不保证最后检查之后或运行期间的外部配置修改。安装版验证仅覆盖所述本机合成组合；不声称已对用户真实项目发出模型请求，不扩大平台或 provider 支持范围。R6-F 多项目并行保持待实施。
