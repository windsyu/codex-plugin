# ADR 0002：默认关闭的只读 App Server Live Adapter

## Context

Store-first rollout 导入能够恢复 durable history，但无法恢复未持久化的流式 delta、运行态和 server request。V1 详细设计允许连接用户显式配置的官方 App Server Unix socket，同时要求默认关闭、raw-first、永不代表用户响应 request，并在退出前解除订阅。官方 App Server WebSocket transport 仍属于实验能力；`thread/resume` 也不是无副作用的纯订阅操作。

本切片以 Codex 源码 commit `41ece455b7fa7166f4fc38522952afdaa2604e18` 和本机 `codex-cli 0.146.1` 为兼容基线，使用隔离的合成 `CODEX_HOME` 完成 initialize、observe-new、loaded-list、resume 失败隔离、resume 成功、unsubscribe 与关闭握手联调。

## Decision

实现 Unix-only、WebSocket-over-UDS 的 App Server Adapter，并保持每个 source 的 `live_mode=off` 为默认值。用户可显式选择：

- `observe_new`：initialize 后只观察新通知；
- `attach_loaded`：分页调用 `thread/loaded/list`，再对每个 Thread 调用无 overrides 的 `thread/resume`。

所有收到的 notification、response 和 server request 先脱敏并作为 transient raw event 提交，再更新 Thread/Turn/Item 与 pending-request 投影。Observer 不发送任何 approval、question 或 turn 控制响应。连接用独立 epoch 表达连续性；断线后 runtime 状态 stale，pending request 标为 source-disconnected。退出时对成功附着的 Thread 发送 `thread/unsubscribe`，随后完成 WebSocket close handshake。

socket 必须是当前用户拥有、权限不宽于 `0600`、位于不可由 group/other 写入目录中的直接 Unix socket。initialize 返回的 `codexHome` 必须与配置 source canonical path 相同，否则 fail closed。稳定 initialize capability 的 JSON 及其 BLAKE3 fingerprint 被记录；正式协议 schema hash 只从兼容清单提供的生成 schema 得出，不用 initialize payload 冒充。

## Alternatives

1. 继续完全关闭 live：安全边界最简单，但无法验证 V1 transient 展示价值。
2. 自动发现并附着任意本机 App Server：降低配置成本，但无法可靠证明 source 身份和 socket 安全性。
3. 对 server request 返回默认拒绝：会改变上游产品行为，越过 V1 read-only 边界。
4. 把 `thread/resume` 描述为纯观察：与官方实现和联调观察不符，会掩盖 loaded 生命周期及恢复副作用。

## Consequences

- durable rollout 仍是默认生产正确性来源，live 只是 opt-in preview；
- `attach_loaded` 可能触发上游 Thread 恢复行为，UI 和文档必须明确告知；
- 未连接期间的 transient 数据不可恢复，不能标记成 `live_complete`；
- 单 Thread resume 失败只降低该 Thread，不能使整个 source 失败；
- Windows 暂时保持 store-only；
- 新 Codex 版本发布 live 兼容性前，需要更新 schema hash 并重跑官方双连接、pending request、resume 和 unsubscribe 回归。

## Status

Accepted

## Date

2026-08-14
