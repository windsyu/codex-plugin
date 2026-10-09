# V3 P2 单题文字回答（2026-10-09）

[验证索引](README.md) · [V3 实施计划](../v3-implementation-plan.md) · [接入契约](../v3-telegram-channel-contract.md#52-必要内容与-typed-answer) · [前片 callback 记录](v3-channel-callbacks-2026-10-09.md)

## 1. 本片结果与边界

本地源码新增非秘密追问的单题 Reply/Other 文字回答、混合选项/文字的完整 typed answer 与无正文 v4 收件。多题只在全部回答齐全后一次写入原生。P2 尚未整体验收；独立排队取消、精确原生整轮取消、真实 Telegram 与 P3 仍未完成。

环境为 macOS / Apple Silicon、原版 `codex-cli 0.156.1`、Rust 1.95.0；标准检查使用 Node 22.23.2 和锁定依赖。全部原生子进程隔离 HOME、USERPROFILE、CODEX_HOME、TMPDIR，合成 Bot/model 仅监听 loopback；不使用真实 Telegram、凭证、模型或私人会话。官方只读 checkout 实查 commit `2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4`，不将其视为安装版 binary 的已证实来源。

## 2. 输入、回答与收件契约

- 本机 `interactiveConsent` 仍缺省 false。仅匹配 Accepted Turn ID、own-user 完整生命周期、准确 Thread/Turn/item、连续捕获水位及当前 request 才可发送/回答；不扩大命令、文件审批范围。
- 1–3 题，选项题 2–4 个选项；isOther 允许 Reply 文字，缺省/null/空 options 按公开协议识别为文字题。任一秘密题、非阻塞、定时或未知 schema 整体回本机。Answer 绑定完整 request 身份、原始 question ID 和选项 label；混用其他请求、重复题、部分回答均不能构造原生结果。
- 每次只发一题；确认 bot API 返回的准确 chat/message 后才激活它。Reply 必须来自绑定 user/private chat，回复当前已确认的 bot 消息，并通过日期、bot 身份、generation、task、request、期限和来源复验。转发、其他 bot、外部引用、quote、编辑、未确认/旧提示及未附着文字不能变成新任务。
- 同题文本与按钮共享一次性 Pending，在第一处 await 前取走全部引用。实际 receipt fsync 后才允许保存下一题选择或尝试原生写入；本机 result/error、resolved、终态、撤销、缺口、到期及未知发送不恢复权限。纯文字提示使用 ForceReply，选项题保留按钮。
- 回答最多 1024 UTF-16 单元及 4096 UTF-8 字节，trim 后非空；只允许 LF 作为控制字符，拒绝 TAB/NUL/ANSI/bidi 控制。保留原始空白、Unicode 和换行，不解释为命令；无 Debug/Serialize、无正文 journal。
- 新写 receipt v4 的 QuestionReplyReceived 只保存 bot/chat/用户回复 message/update ID 与 task/interaction UUID。任务须处于相同 bot/chat 的 Submitting；与 Received/Rejected/CallbackReceived 共用身份去重、4096 项及 4 MiB 上限。读取 v1/v2/v3/v4 混合行，旧版本拒绝新事件，旧读取器不兼容 v4；恢复只去重，不重放回答。

## 3. 协议事实与项目选择

固定源码的 [RequestUserInputQuestion](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/app-server-protocol/src/protocol/v2/item.rs#L1740) 允许可空 options，isOther/isSecret 缺省 false；[模型工具解析](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/core/src/tools/handlers/request_user_input_spec.rs#L119) 要求非空 options 并自动设置 isOther。因此无选项题只有公共协议、typed/session/service 合成覆盖，不能据此宣称当前安装版正常模型生产路径可产生无选项题。

原版 TUI 的 [回答 UI](https://github.com/openai/codex/blob/2351d9e1b608e6f9d9a3699b71d7eb39ee41cfa4/codex-rs/tui/src/bottom_pane/request_user_input/mod.rs#L865) 会构造内部 `user_note:` 等标记；公开 typed answer 不要求该 UI 约定。本项目明确选择原 question ID 下 `answers: [原始选项 label 或原始文字]`，不添加 UI 标记。实际原生工具输出须验证这一选择，不能只凭 parser 测试宣称被模型接收。

Telegram 的 [Message](https://core.telegram.org/bots/api#message) 和 [ForceReply](https://core.telegram.org/bots/api#forcereply) 文档用于核对原消息身份及回复 UI；[InaccessibleMessage](https://core.telegram.org/bots/api#inaccessiblemessage) 的 date=0 不授予回复资格。未读取被回复正文来推断权限。

## 4. 原生专项与失败记录

受管 artifact `1a146376-1a39-4146-968b-88f972d9afd8` 为 failure：渠道 200/200、控制 17/17（2 ignored）与原版 P1 1/1（3.41 秒）通过；新增 P2 第三轮失败，不能计为通过。后续定位确认 fixture panic hook 覆盖首个断言，最终记录的是模型请求上限；原版 TUI 的 Plan 模式检查已通过。修正为保留首个失败位置及有限静态布尔诊断，不扩大请求上限、不输出原始 payload、不改变生产控制。

后续失败 artifact `6beebad7-2e26-4e85-9ea4-394214678a03` 和 `53952b69-0e29-4127-8431-3a0cd1a74795` 保存了首次断言：fixture 额外要求模型请求必须声明该工具，安装版返回的声明未满足这一检查；显式配置后仍如此。固定源码支持 `tools.experimental_request_user_input.enabled=true`，但不能用该源码推断安装版声明或配置开关的实际效果，也没有证据说明模式未继承。移除这项未经安装版验证的额外先决断言，继续要求真实原生提问和完整工具结果；实际拒绝仍会导致失败，没有放宽回答验收。

原生专项 `installed_cli_production_telegram_interaction_path` 的 artifact `fa592fbb-0fe5-4302-a98b-4d3f7b53f110` 为 success，1 passed / 0 failed，6.22 秒。经过 Application owner 配置/配对/确认、生产 polling/execute actor、原版 remote TUI 与同一 loaded Thread；合成模型提供真实工具调用，未伪造原生 request 或通用 RPC 答案。

前两轮仍验证命令 Allow once 与单文件 Add Deny。第三轮使用原版 TUI `/plan`，隔离原生配置显式启用实验追问工具，再由渠道发起两题请求：第一题点击原始选项，第二题明确 Reply 原始 Unicode/换行文字。原生工具结果必须包含两个原 question ID 下的准确答案；答案未齐前模型不能继续，文字 receipt 在继续前已实际持久存在，journal 不含答案或问题正文。错误用户、其他 bot、无 Reply、旧题、重复和已退休提示均不产生新任务或第二次回答；同一 Thread 上下文与 final-only 结果保留。

这是原版 CLI 加合成 Bot/model 的行为证据，不证明正常真实模型一定声明或发起该工具，也不证明未启用实验配置的行为。公共无选项题与人的真实同时输入仍未受测。

## 5. 最终检查与剩余验收

标准检查 artifact `43873496-f0f0-416c-9d07-a7bd58423262` 为 failure：80 项维护工具、271 项前端测试、typecheck、前端构建与 formatter 通过，Clippy 的 `-D warnings` 拒绝只被测试引用的 Answer.question 访问方法。删除该多余方法及无行为验证价值的访问断言，保留准确完整答案断言；重新运行标准检查，不降低门槛。

标准 `node scripts/dev.mjs check --offline` 的最终 artifact `ca57fcb7-eb5a-491a-a4c1-1c472ae556d5` 为 success，使用固定工具链和锁定依赖：

| 检查 | 结果 |
| --- | --- |
| 文档链接/结构 | 115 份，0 错误；最终交付更新后再复验 |
| 维护工具 | 80 passed / 0 failed |
| 前端 | 271 passed / 0 failed，typecheck 和构建通过 |
| Rust all-targets | 676 passed / 0 failed / 61 ignored |
| formatter / Clippy | 通过，保留 `-D warnings` |
| debug / release 两个 binary | 构建通过，Web 资源先重建 |
| Git 二进制策略 | 实际暂存区 24 项、待提交树 706 项，均 0 错误；最终文档保存后复验 |

全量检查后，最终受管 artifact `2df78051-d991-4102-a2ba-3ec999b03908` 为 success：显式执行原版 CLI 的 P1/P2 专项，各 1 passed / 0 failed，分别 3.61 与 6.21 秒。它们在标准检查中属于 ignored，单列实际结果；不将其余 ignored 当作通过，也不重复加总定向测试。所有本片生产源码在最终检查前冻结。

独立 GPT-6 对 typed/session/receipt 及 service/Bot 接入进行只读审查，未发现必须修复的授权或写入问题；审查未运行测试，不能代替验证。已更新未附着文字与后续问题提示，使其说明 Reply 路径。随后补充仅测试用的既有 worker 暂停包装：生产 question_reply 在收件 await 处暂停时已消费全部按钮；竞争按钮持久拒绝，期间本机回答或撤销使恢复后的原生写入为零，正常路径准确写入一次。artifact `e4d88fc6-3c59-4798-ba43-f7e15f55627f` 的 service 23/23 通过，1.79 秒；不将这三种确定性路径表述为真实 Telegram 或完整网络竞速矩阵。

旧数据库、migration、默认 V2 入口、旧只读 `/v1` 和官方 CLI 不变；本片改变独立 receipt 新写格式，旧读取器不兼容 v4。没有远端 push、Issue/PR、合并、发布或私人数据上传。本片 Tailscale 只读报告监听因自动审批要求具体人类授权仍未开放，已有 P0 地址不能算本片验收入口。

## 6. 用户验收入口与下一片

本机操作遵循[使用说明](../guides/usage.md)：显式启用 channel-ready，在本机面板配置自己的 bot、配对准确私聊和已有 Thread，并另行勾选必要交互权限；不要把凭证贴入报告或会话。下面是待用户执行的真实验收，不是已通过结果：

1. 原生能产生非秘密多题追问时，确认私聊每次只显示一题；点击第一题选项，第二题用 Reply 回答当前提示，文字可含 Unicode 和换行。
2. 观察完整回答后原生才继续；最终回复回到准确私聊，本机自行发起的任务不自动外发。
3. 回复旧问题或未附着文字，确认没有新增任务；本机先回答或撤销后，旧远端回答不得继续执行。
4. 若真实模型不提供追问工具，记录该能力缺口，不能用本片合成模型成功替代。需要同时操作、故障和跨设备证据时，按 P3 范围继续验证。

下一片最小任务为独立排队取消，须先与提交意图 fsync 的竞争建立明确接口；不能将取消排队解释为取消已提交原生 Turn。阶段状态只维护在 V3 计划。
