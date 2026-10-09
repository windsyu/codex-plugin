# V3 P2 独立排队取消：提交竞争与原子收件

> 2026-10-09 实施；2026-10-10 提交前复验。此记录只覆盖本服务尚未提交的准确渠道任务；精确已提交 Turn 取消与真实 Telegram 验收仍未完成。阶段状态只在 [V3 计划](../v3-implementation-plan.md)维护。

[接入契约](../v3-telegram-channel-contract.md#54-独立排队取消) · [ADR 0055](../decisions/0055-v3-channel-native-protocol.md) · [前一片文字回答](v3-channel-question-replies-2026-10-09.md)

## 1. 用户结果与实现边界

原生目标忙碌或前面还有渠道任务时，已绑定私聊收到静态等待提示。前一任务已原生完成、执行器仍等待最终发送确认或收件收尾时，新任务也能立即获得提示；内存标记只表示执行器仍占用，不恢复任何授权。bot API 确认准确私聊消息后，绑定使用者直接发送提示中的 `/cancel <32位小写十六进制引用>`，只取消该任务。提交权已取得时关闭引用，即使原生字节尚未写入也不能再通过它取消。Reply 继续按当前问题处理，不将答案里的命令转成取消动作。

取消和提交在同一 Core 锁中分别从 Open 取得 SubmitClaimed 或 CancelReserved；普通结束另有 Finished 状态。取消获胜后移除队列正文，已弹出的等待 actor 也不能提交、独立关闭或覆盖取消状态。失败、丢失返回或未知结果不恢复权限、不自动重试，不宣称已成功取消。每个任务明确返回收件关闭归属；最近任务 UI 状态不能决定另一任务的不确定收件是否关闭。

一个受管 worker 发送取消提示和静态确认，polling 与 native actor 不等待发送。引用/outbox 各最多 17 项，最多一条 HTTP 在途；确定未尝试的提示可在短暂积压解除后重新提供，未知发送不重新尝试。撤销和失效清除权限及待发内容，迟到确认不激活旧引用。在途去重身份另有 17 项上限，不因撤销或重新绑定而变成可重用身份；操作返回后释放，丢失调用者时保守保留到服务结束。生产 polling 仍串行，忽略在途重复不构成独立的持久成功 ACK。

原生 CLI、默认 V2 入口、旧只读 `/v1` 和原生 durable history 不变。此路径不写入 `turn/interrupt`，不锁住 TUI，不恢复旧控制内核。没有配置 schema 或旧数据库 migration；独立 receipt 新写格式改变。

## 2. 收件格式与恢复

当前新写 journal v5，兼容读取 v1/v2/v3/v4/v5 混合行；旧版本不得包含 QueuedCanceled，旧读取器不能读取 v5。不重写历史行。新事件仅有：

```json
{"kind":"queued_canceled","key":{"bot_id":101,"chat_id":202,"message_id":20},"update_id":20,"task":"00000000-0000-4000-8000-000000000001"}
```

准确任务必须仍是同 bot/chat 的 Received；一行合并取消输入去重与 Received→Closed，经实际 fsync 后才确认。与 Received、Rejected、CallbackReceived、QuestionReplyReceived 共用 update/message 身份和 4096 项/4 MiB 上限。已消费的输入不能夺走另一引用，也不学习冲突的第二身份。取消引用、任务/命令/回答正文、原生 RPC/Thread ID、路径和 token 不保存。恢复只去重和统计 Closed，不恢复正文、引用或命令。

## 3. 验证条件与初轮失败

固定 Rust 1.95.0、Node 22.23.2、锁定依赖；原版 CLI 0.156.1。合成 Bot/model 仅监听 loopback，原生测试显式隔离 HOME、USERPROFILE、CODEX_HOME、TMPDIR；通过 kernel peer/组/birth identity 与 socket inode 保护自身进程回收。没有真实 Telegram 凭证、真实模型或私人会话。

初轮受管 artifact `baf9424b-c36f-4a7d-a271-53cc05735a4b` 为 failure：前端构建通过，Rust 编译因 Binding.snapshot 需要可变借用而失败。修正 candidate 的借用，不降低检查。

第二轮 artifact `d29d9aaa-69c9-423c-852f-1d0cce14419f` 为 failure：渠道 223/223 通过（4.10 秒），原版专项在 fresh 阶段的恢复检查失败，不能计为通过。夹具只复制 journal，违反生产 Store 的缺失锁文件保护；修正为先创建合法 Store 锁/journal 对，再覆盖私有快照，并等待新任务 Closed 后复制。不放宽生产完整性校验。

第三轮 artifact `58b74cad-87d7-4604-9e45-a6bbcd7dfc92` 为 failure：新增确定性回归后渠道 224/224 通过（4.10 秒），原版恢复夹具的初始 Store 仍失败。隔离根目录未满足私有父目录要求；将快照放入明确 0700 的隔离 HOME，保留合法锁/journal 初始化。两个夹具前提均已修正，后续成功复验见下文。

回归复验 artifact `6fa8c3c4-632a-4566-96a6-b031c6522518` 为 failure：原版 P1 1/1 通过（3.75 秒），P2 文字专项仍要求旧新写版本 v4，遇到 v5 而失败（首次 fixture_line=51）。把当前写入格式断言同步为准确 v5，保留完整答案、摘要和隐私断言；本次失败不计通过，queue 专项因前项失败尚未执行。

独立 GPT-6 源码审查发现并促成三项修正：旧 actor 更新不能覆盖取消状态；最初分类也必须保护已移走引用的在途重复，且保留跨 generation 的在途去重；冗余资格检查不能因短暂捕获 Backlog 关闭合法任务。最后一项撤回了新增检查，保留原有 arm/submit 来源与目标门。审查没有运行 Cargo，不能代替行为验证。

## 4. 最终自动化检查

执行占用修正前的原版专项基线 artifact `3e1b5cc2-4468-48f8-89e1-396f2255a9ad` 为 success，显式执行通常 ignored 的三个用例，各 1 passed / 0 failed：

| 专项 | 时间 | 实际证明 |
| --- | --- | --- |
| P1 文本 | 3.68 秒 | Application 配置/配对/确认、同一 Thread 上下文、final-only、错误身份与重复排除 |
| P2 交互 | 6.11 秒 | 命令单次允许、单文件 Add 拒绝、混合多题选项+Reply、准确原始答案与 v5 无正文收件 |
| P2 排队取消 | 3.73 秒 | 取消 popped waiting 与 queued 两个任务；两条 QueuedCanceled 先于确认发送，取消任务没有 Submitting；延迟首个确认期间 polling 和本机原生任务继续；本机 final 未外发；释放确认后新渠道任务沿用同 Thread，并由生产 Store 恢复准确 Closed |

排队取消专项要求准确静态提示和确认，共 2 个提示、2 个确认、1 个新任务最终回复；普通模型请求共 4 个。错误使用者、未知/畸形引用、转发、已消费引用持久拒绝，重复身份不产生新任务。新任务是在释放延迟确认后完成，不扩大为所有并发/故障条件的保证。

该基线渠道定向 224 项通过，其中新增 13 项 service 取消回归覆盖双重暂停存储、提交先取得权、未知/迟到 HTTP 确认、跨撤销重绑的重复身份、另一引用保护、旧 UI 更新及 17 项边界。Store/worker 测试另覆盖实际 fsync 失败、不完整尾部、丢失结果、异常退出、混合旧版本及共享容量；不能把单列与全量重复加总。

标准检查初次 artifact `b19df16e-5737-4b20-9361-e2bee62b6c87` 为 failure：116 份文档检查发现两处历史取消章节 anchor 失效，后续检查未执行。为重编号章节保留旧 anchor 后重新运行，不删除历史证据链接。

标准检查 artifact `d3393e76-8cba-4987-9ba7-f6529cbe1552` 为 failure：116 份文档、80 项工具、274 项前端、typecheck/构建、fmt/Clippy 通过；Rust library 为 588 passed / 1 failed / 57 ignored，后续 all-targets 和 debug/release 未完成。失败是既有历史清理夹具 `cleanup_global_lock_replaced_targets_corrupt_policy_and_changed_revision_fail_closed` 对默认 umask 的隐含依赖：为保护诊断日志设置 077 后，空替换目录变为私有 0700，生产逻辑安全跳过，夹具却期待非私有目录错误。定向 artifact `715668ae-72ca-45f9-bb37-d5cb6f57cd5f` 在默认 umask 下 1/1 通过；显式 077 的 `d8f4c366-2ca7-41dd-98dc-2052dd062d8f` 同一断言复现失败。仅将该负例的替换目录明确设为 0755，保持权限拒绝与原目录保留断言，生产清理逻辑不变。修正后的定向 artifact `d031f2f6-b109-41b3-9a13-5da5b2642bae`（077，0.22 秒）和 `d74b44db-6170-4c42-9bc2-ce21d35fde13`（默认，0.15 秒）各 1/1 通过。

随后标准基线 artifact `62892e45-781b-431c-9d7b-97c0be7f9435` 为 success：116 份文档、80 项工具、274 项前端通过；typecheck、前端构建、fmt、Clippy、Rust all-targets 共 699 passed / 0 failed / 62 ignored、两个 binary 的 debug/release 构建通过。清理夹具修正另存为本地提交 `0a01496`。这轮早于下述执行占用修正，不能作为最新源码的最终全检。

最后审查补齐原生已空闲但前任务仍等待最终发送确认的窗口，新增第 14 项 service 回归及原版 CLI 慢最终 ACK 场景。修正后定向 artifact `b3da9b85-7b3c-471f-a20b-f5ead726e8ec` 为 success：前端构建、渠道 225/225（4.13 秒）与原版排队取消专项 1/1（4.40 秒）通过。

最新原版专项 artifact `cc082ca5-efed-42a3-b96f-247842de8453` 为 success，显式执行三个通常 ignored 的用例，各 1 passed / 0 failed：P1 文本 3.51 秒、P2 交互 6.22 秒、P2 排队取消 4.16 秒。前两项保留上述断言；排队取消扩展后要求 3 个准确提示、3 个取消确认、1 个最终回复，共 7 次发送；模型请求仍为 4 个，收件为 4 个 Received、3 个 QueuedCanceled 和 1 个 Submitting。最后一个取消发生在前任务原生已空闲、最终 HTTP 确认尚未释放时，新增任务取得已确认引用、没有 Submitting 或新增模型请求；释放确认后，生产 Store 将三个取消任务准确恢复为 Closed。独立 GPT-6 审查核对该夹具、实际日志与成功元数据，确认未用 Session/Core 控制捷径；此证据仅覆盖该延迟场景，不代表全部并发 HTTP 或故障条件。

最新源码标准全检 `node scripts/dev.mjs check --offline` 的 artifact `9eca9409-2790-485c-8a76-8f1b1fb999c9` 为 success，完成时间及 lease 已核对：116 份文档、80 项工具、274 项前端，typecheck/前端构建、fmt、Clippy 均通过；Rust all-targets 的 12 个结果组共 700 passed / 0 failed / 62 ignored。两个 binary 的 debug 构建 0.09 秒、release 构建 20.83 秒通过。上述三个 ignored 原版专项单列，不与常规测试加总，不将剩余 ignored 用例计为通过。主 Cargo 缓存约 42 GB 仅提示，未清理缓存、未登记资料或用户历史。

最终复验：116 份文档、工作树/暂存区差异检查通过；暂存区 22 个文件与候选提交树 710 个文件的二进制策略检查均为 0 errors。只提交渠道运行/收件、合成回归、静态 UI 状态与同步文档，没有凭证、私有会话或构建产物。

## 5. 用户验收与剩余任务

按[使用说明](../guides/usage.md)在本机显式启用 channel-ready、配置自己的 bot、配对准确私聊并确认已有 Thread。以下为待用户执行的真实验收：

1. 本机原生任务运行时，从 Telegram 发两条任务；分别取消已弹出等待的任务和仍排队的任务，确认收到静态取消结果，原生不会执行这些正文。
2. 重发同一取消消息、旧引用、错误私聊或转发命令，确认没有第二次取消或新任务。
3. 提交已开始后尝试旧引用，确认不能中断已提交任务；本机按原生方式处理取消。
4. 取消确认延迟或断线时核对本机状态，不将没有确认解释为已完成；随后新任务仍沿用已绑定 Thread。
5. 前任务原生已完成、最终发送确认仍未返回时，再发新任务，核对其等待提示与取消结果；没有提示不能猜测引用或视为已取消。

精确整轮取消的公共原生竞态缺口、真实 Telegram/用户同时输入、完整 P2 故障矩阵、P3 及跨会话管理仍未完成。未 push、创建 Issue/PR、合并、发布或上传私人数据。Tailscale 新报告监听仍需具体地址的人类授权；不能把已有 P0 只读入口当作本片验收入口。
