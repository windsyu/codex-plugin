# ADR 0049：工作台独立用户目录

- Status：Accepted
- Date：2026-09-20

## Context

用户要求默认历史和配置按结构统一存放于用户目录下的 `.codex-web`，并明确 Windows 使用 `%USERPROFILE%\.codex-web`，与 macOS/Linux 同名。此前 ADR 0044/0048 将默认历史和配置分别置于 `$CODEX_HOME/workbench-data-v1` 和 `$CODEX_HOME/workbench/config`，会随原生 CLI 的 home 改变，也不便于识别工作台拥有的数据。

## Decision

1. macOS/Linux 根目录为 `$HOME/.codex-web`；Windows 路径约定为 `%USERPROFILE%\.codex-web`。共用 `config/config.json` 和 `history/` 布局，历史内继续按 `runs/<runEpoch>/` 保存日志、快照和上下文。`config.schema.json`、配置备份/锁位于 `config/`，历史索引/占用缓存/清理任务位于 `history/`。
2. 默认值解析独立于 `CODEX_HOME`。原生 CLI 仍使用原有 `CODEX_HOME`（未指定时为原生默认 `.codex`），不复制、修改或搬迁其认证、配置及会话。
3. `--config-dir`、`--data-dir` 及 JSON 的 `storage.dataDir` 保持原有含义和优先级。`storage.dataDir: null` 现在表示新默认历史目录。覆盖配置目录不改变默认历史目录；覆盖数据目录不改变默认配置目录。
4. 只更换默认值，不自动迁移、合并、复制、删除或回退读取旧目录。已有绝对路径设置继续按保存值使用；需要继续使用旧配置/历史时显式指定旧目录。新目录没有旧配置时采用默认关闭删除的安全初值；旧记录仍在原位，不会自动出现在新目录的历史列表。
5. 根目录及子目录采用既有私有权限和不跟随链接的文件访问约束。配置默认根在加载配置时建立；仅使用默认历史、配置另行指定时，由异步记录器建立根，保存故障继续以降级状态表达，不进入网络/PTY 热路径。没有新增后台迁移或目录扫描。
6. Windows 的目录选择逻辑和共用结构可独立测试；当前工作台的 PTY、信号和文件锁实现仍依赖 Unix。本次不宣称完整 Windows 运行支持，也不以目录约定代替 Windows 验收。

## Alternatives

- 继续依赖 `CODEX_HOME`：不符合用户指定的独立目录，也会将工作台偏好绑定到原生配置目录。
- 自动搬迁旧记录、合并两处历史：涉及活动运行、原有清理策略和多进程并发，超出更改默认位置的请求；显式路径仍能继续读取旧记录。
- 按平台分别采用 XDG/AppData 子目录：增加用户查找成本，不符合各平台同名、同结构的决定。

## Consequences

首次使用新默认值时会生成独立配置和历史。原目录不丢失，但不会自动合并到新列表；继续读取旧记录需显式指定原数据目录。JSON 字段、记录格式、API 和清理保护边界不变，没有数据格式 migration。测试必须显式注入临时用户 home，真实 binary 的测试子进程使用独立临时环境，避免新的默认值写入真实 `~/.codex-web`。

本决定替代 ADR 0044/0048 的默认路径部分，其余契约继续适用。目录与兼容说明见[历史与配置方案](../codex-native-cli-workbench-history-settings.md#21-固定目录结构)，实施状态见[R3.1 计划](../v2-implementation-plan.md#r31-history-settings)。
