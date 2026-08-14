# ADR 0005: Single DbWriter and CommittedEventBus

## Context

Scanner、Live Adapter 与维护状态若各自打开 SQLite 写连接，会在规模增长后争抢 writer lock；轮询 SQLite 也无法表达“事务已经提交”的精确边界。

## Decision

`serve` 启动一个专用 DbWriter thread，持有唯一写 connection。scanner 和 Live Adapter 通过 4096-event 有界队列提交；控制写使用独立有界队列。DbWriter 最多合并 100 event 或等待 50ms，并只在事务成功后向容量 512 的 CommittedEventBus 发布最新 sequence。API 使用最多 8 条 read-only connection；SSE/WS 先订阅 bus，再从数据库 replay 到边界，随后进入 live。lagged consumer 收到 `SLOW_CONSUMER` 后断开。

## Alternatives

- 每个 adapter 直接写 SQLite：实现简单，但锁竞争和 checkpoint 正确性难以统一。
- 固定间隔轮询：不会漏 durable 数据，但延迟、负载和 snapshot/live 切换边界较差。

## Consequences

背压会阻止 scanner 推进 checkpoint，并让 Live Adapter 停止继续读取 socket。提交失败不发布事件，未提交 batch 可由事实源重试。单进程锁仍用于拒绝第二个 daemon。

## Status

Accepted

## Date

2026-08-14
