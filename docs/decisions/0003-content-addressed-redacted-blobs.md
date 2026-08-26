# ADR 0003：内容寻址的已脱敏 Blob 存储

## Context

Rollout 和 App Server event 可能包含大型命令输出、diff、工具结果或媒体编码。若始终把完整 JSON 内联到 `raw_events` 和 projection，SQLite、FTS、API replay 与浏览器 Raw Inspector 都会承担不必要的容量和内存压力。V1 仍需要保留脱敏后的原始事实、支持 projection 重建，并保证 retention 不产生悬空引用。

## Decision

当已脱敏 raw JSON 超过 `capture.inline_blob_bytes` 且未超过 `max_raw_event_bytes` 时，Observer 按其 BLAKE3 `stored_hash` 生成 `blob_<hash>`，将内容写入 `storage.blob_dir/<hash-prefix>/<hash>.json`。写入顺序为 temp file、file fsync、atomic rename、directory fsync，再在 raw-event 事务中插入 blob metadata 和引用。文件和根目录分别使用 `0600`、`0700`；读取使用服务端记录的 relative path 和 `O_NOFOLLOW`，不接受客户端 filesystem path。

`raw_events.raw_json` 保存不可执行的 blob-reference stub，`raw_events.blob_id` 指向完整脱敏 JSON。大型 Item projection 只保存 blob reference 与独立 summary，不复制完整 payload。`blob_references` 分开记录 raw-event 和 projection 引用；projection 更新或 rebuild 会替换对应引用，raw retention 先删除过期 raw 引用，blob retention 只删除无剩余引用且超过宽限期的内容。DB commit 前 crash 遗留的文件由启动 orphan sweep 在一小时宽限后删除。

`GET /v1/blobs/{blobId}` 与其他 V1 API 使用相同 bearer 和 Origin 策略，只返回仍有引用的 blob。响应强制 `Content-Disposition: attachment`，仅允许单一 byte range，并以四个并发下载为上限。HTML/SVG 等类型不会作为主动内容 inline；当前生成的 raw blob 固定为 `application/json`。

## Alternatives

1. 所有 raw JSON 继续内联 SQLite：实现简单，但大 payload 会放大 WAL、查询和 stream 成本。
2. 只在文件系统保存 raw，不记录 DB 引用：无法事务化 checkpoint/projection，也难以安全 retention 和授权查询。
3. 使用用户原始文件路径作为 blob：会把 API 变成任意路径读取面，并破坏 Observer 数据独立性。
4. DB commit 后再写文件：可能提交永久悬空引用，重建和下载无法可靠恢复。

## Consequences

Schema 10 起使用 `known-secrets-v2`。除结构化 secret 外，URL token/signature、HTTP/MCP auth 与 credential/JWT/PAT 形态在 blob 决策前统一脱敏；明确的 image/audio base64 正文只留下 media type、估算大小和 keyed fingerprint marker。历史 v1 raw/blob/projection 不自动重写，health、Viewer 与 export 必须持续警告 legacy 数量。

- blob rename 后 DB commit 前 crash 可能留下 orphan，但不会留下已提交的悬空引用；
- projection rebuild 需要读取并校验 blob hash，缺失或篡改会 fail closed；
- raw retention 后，只要 projection 仍引用 blob，文件继续保留；
- blob 下载是脱敏原文下载，不是原始 Codex 文件下载；
- `max_raw_event_bytes` 以上的 record 仍只保存带 fingerprint 的 decode-error placeholder，不把无上限输入写入 blob；
- schema 升级到 version 5。

## Status

Accepted

## Date

2026-08-14
