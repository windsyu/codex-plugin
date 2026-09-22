# ADR 0040: 修复宽字符 resize 并隔离屏幕重建故障

## Status

Accepted。这是 R1 浏览器验证发现的可靠性修复，不改变切片通过条件。

## Date

2026-09-19

## Context

真实安装版 CLI/Chrome 验证在 1024、736、320px 视口切换后触发 `vt100` 0.16.2 越界：`Grid::set_size` 经 `Row::resize` 截掉宽字符的后半格，后续清行在 `Row::clear_wide` 访问不存在的格子。最小回归先在未修改的 crate 上复现；[上游 issue 28](https://github.com/doy/vt100-rust/issues/28)描述同一缺陷。

此前 VT 重建和 PTY 在同一 actor 中，解析器 unwind 会销毁 PTY owner，连带停止原生 CLI。屏幕快照属于观察副本，其故障不应决定 CLI 的生命周期。

## Decision

1. 以 `[patch.crates-io]` 使用项目内 `vendor/vt100`，保留 MIT LICENSE、发布版本和[补丁说明](../../vendor/vt100/PATCH.md)。唯一第三方源码修改是在 `Row::resize` 清除被截断的宽字符首格并保留属性，使活动/非活动屏幕都满足边界条件。没有升级依赖、修改 Cargo cache 或修改原版 Codex。
2. 新工作台用 [ScreenJournal](../../src/workbench/terminal/screen.rs)封装重建。捕获重建的 unwind 后立即丢弃该解析器；不再次调用损坏状态，不重放已发送按键或重建 CLI。
3. 现有页面继续收到完整有序的、经终端安全过滤后的实时字节；PTY 输入、resize、模型转发及原生退出继续工作。报告带 epoch/outputSeq 的 `screen_state_unavailable`，不能静默吞掉故障。
4. 重连页面收到明确不完整的空快照和故障状态，只能显示后续输出；不冒充已恢复当前屏幕。无有效光标证据时不生成 CPR 回复；固定能力与颜色回复继续提供。UI 保留故障提示，输入授权不能清除该提示。

## Alternatives

- 在 resize 前注入清行 ANSI：可能改变尚未解析完的 UTF-8/转义序列、光标、模式和备用屏幕，不采用。
- 仅捕获 panic 后重建空屏：能保住 CLI，但正常窄屏操作每次都损失屏幕，不能替代已知缺陷修复。
- 更换整个终端实现或跟随未发布 Git 分支：扩大协议和快照回归范围；当前选择固定版本的最小本地补丁。

## Consequences

需要维护一份第三方源码补丁；上游发布修复后，只有原回归、快照/模式及真实浏览器检查全部通过才可移除。`catch_unwind` 只隔离可展开栈的解析故障，不能承诺隔离进程 abort、OOM 或所有 CPU 卡死。故障后的新页面没有可靠历史屏幕，必须保留提示，不能自动重启 CLI 来掩盖缺失。

验证包含正常/备用屏幕的中文与 emoji 截断、恢复后清行，以及故障注入后输入仍只执行一次、输出序号连续、同一子进程继续运行。浏览器与完整回归结果记录于 [R1 阶段证据](../validation/native-cli-r1-progress-2026-09-18.md)。
