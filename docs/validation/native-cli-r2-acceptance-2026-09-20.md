# R2 验收：工具与请求阅读

日期：2026-09-20。**R2 在以下受测范围通过，暂停供用户试用；未开始 R3。** 完整 V2 尚未完成，唯一切片状态见[实施计划](../v2-implementation-plan.md)。本报告汇总分阶段证据，不把所有历史测试写成今天重新运行。

## 1. 验收基线

项目为 `codex/native-cli-live-workbench` 上 `930172493c163a903621ca14c540d1ca10dd4f3e` 加未提交工作树；当前正式 binary SHA256 `357c5e41acd7eec32e96bed47e609683922e42490dc8a4ed0bd199b95fd1898b`。安装版 CLI `0.154.0`、本机 Chrome `153.0.8010.50`，参考官方源码 commit `633ab199cfd724aa78013c006b27a2b3d049fc3b`。

[统一 ViewItem 回归](native-cli-r2-view-items-2026-09-20.md)使用该 binary 验证真实 CLI 的读写/失败、文件 Diff 与结果、迟到命令/轮询/长输出及缺口恢复；[本次上下文和生命周期](native-cli-r2-contexts-2026-09-20.md)继续使用同一 binary。所有 R2 原生验证均由合成模型驱动，真实执行只发生在临时项目；真实 provider 支持沿用 R0/R1 已验收范围，不因合成模型的配置名称、WS fixture 或缩窄视口而扩大。

## 2. 通过条件逐项核对

| 条件 | 证据与结果 |
| --- | --- |
| C03 上下文/同文再次提交 | 原生 compact/fork/resume 保留模型历史；当前 Run 仅产生有原生提交身份的新用户气泡。同轮 steering 保留两条不同提交，不按文字去重，不把观察顺序冒充跨来源全序。[上下文验证](native-cli-r2-contexts-2026-09-20.md#native-context) |
| C04 辅助请求/用途未知 | 受测 metadata 分类；标题/压缩不混入聊天。完整请求详情按需读，WS create/response 分开，缺少关联仍可在请求级查看。[请求详情](native-cli-r2-request-details-2026-09-19.md)、[并发验证](native-cli-r2-contexts-2026-09-20.md#concurrency) |
| C05 调用无结果 | function/custom 参数真实流式出现，最终全文原位替换；生成完成不变执行成功。原生取消参数流后显示不完整/未观察到执行。[工具验证](native-cli-r2-tools-2026-09-19.md)、[生命周期](native-cli-r2-contexts-2026-09-20.md#native-lifecycle) |
| C06 原生命令结果 | 真实读取/编辑、退出 0/7、最后模型请求后的迟到退出、write_stdin 及长输出按明确来源补原卡；无可靠事实的错误输出只标 result_observed。[规范结果](native-cli-r2-native-results-2026-09-19.md)、[当前构建回归](native-cli-r2-view-items-2026-09-20.md#3-自动化与浏览器结果) |
| C07 文字/代码/命令 | Markdown shell 代码仍是模型文字；Code Mode 为代码工具卡，不扫描代码生成子命令；已确认 schema 的直接命令分区呈现 cmd/cwd/结果。[工具验证](native-cli-r2-tools-2026-09-19.md#2-用户可见结果) |
| C08 并发/逆序/重复 | 相同 WS call/wire ID 在不同响应中不串接；HTTP 逆序和重复结果只更新原卡，展开、焦点、上滚锚点保持；刷新不重复或另开 CLI。[并发验证](native-cli-r2-contexts-2026-09-20.md#concurrency) |
| C09 不完整/冲突/取消 | 缺身份/未知类型有安全提示，冲突撤销确定状态；响应完成不结束任务。原生拒绝、未知工具与参数错误可读；turn_aborted/EOF 无逐工具终态时保持未知。[统一契约](native-cli-r2-view-items-2026-09-20.md)、[生命周期](native-cli-r2-contexts-2026-09-20.md#native-lifecycle) |
| C10 安全/可用性 | HTML/ANSI/已知秘密和媒体/加密内容受控，输出/上下文有预算及截断提示；键盘展开、窄屏、流中上滚、焦点和终端节点已验。来源字段不靠颜色表达。[请求详情](native-cli-r2-request-details-2026-09-19.md)、[统一契约](native-cli-r2-view-items-2026-09-20.md)、[本次验收](native-cli-r2-contexts-2026-09-20.md) |
| 修改与结果 | apply_patch 新增/修改/移动/删除的拟议 Diff 可展开；规范 FileChange 成功/执行失败和 stdout/stderr 原位补齐，校验失败不猜成败。[拟议 Diff](native-cli-r2-proposed-diff-2026-09-19.md)、[规范文件结果](native-cli-r2-file-results-2026-09-19.md) |
| 请求和用量边界 | 认证读取、固定字段、省略/截断、分页和失效 cursor 有契约/负向测试；工具定义支持 namespace、顶层 tools 与 developer additional_tools。usage 按明确响应展示，不合计成任务用量。[请求详情](native-cli-r2-request-details-2026-09-19.md)、[并发验证](native-cli-r2-contexts-2026-09-20.md#concurrency) |

上述满足 R2 的实际读写与失败可核对、工具/模型/命令区分、重复不追加、并发不串接和未知来源可阅读条件。首次明确 typed replace、统一 schemaVersion=2、最终替换与快照缺口恢复另见 [ADR 0043](../decisions/0043-unified-reading-items.md)。

## 3. 必须保留的限制

- **事实来源。** 原生试验未取得可靠 started 或 declined 终态；规范映射有 fixture 测试，实际 UI 只显示已取得的事实。本轮实测策略拒绝和 Esc 均没有相应逐工具终态，所以保持未知是 C09 的正确结果。不会为了填满状态而更改 CLI、添加审批入口或把文字错误升级为执行事实。
- **并发关系。** WS create→新 response、无明确父调用的 Code Mode 子命令、同轮用户与模型的跨来源精确顺序不推断。请求级原文和原生同轮记录仍可阅读。
- **阅读范围。** 单次模型响应结束不是原生任务完成；截图不证明全量捕获。参数、结果、schema、媒体与加密内容可能省略/截断。秘密识别有已知格式边界，不承诺解释任意代码编码。
- **后续阶段。** 磁盘记录、持久水位、服务重启恢复归 R3；文件/搜索/Git 工作区 Diff 归 R4；新增真实 provider/设备/IME/图片矩阵、完整性能故障验收和旧控制退役归 R5。

这些是已确认设计中的证据边界与后续范围，不是未实现的第二套对话/审批流程。尤其“原生 declined 待验证”不再被当作必须制造该状态的实现任务；只有未来来源实际提供此终态时才扩大实际能力声明。

## 4. 最终检查与交付

当前库测试 158 passed / 23 ignored；4 组新增 CLI/Chrome 场景已分别显式通过，pageErrors 均为 0。cargo fmt、包含 tests 的 Clippy 和新增探针语法检查通过；6 份文档的 215 个本地链接/锚点、标题/代码块与变更空白检查通过。前端生产代码未变，沿用同一构建在统一 ViewItem 增量中的 133 项测试、TypeScript/Vite 和 binary 构建证据。已有工具/Diff/长输出回归通过后没有相关生产变更，因此未无理由重复整套用例。

无数据库 migration、旧 `/v1` 变化、用户全局配置修改、用户调试服务重启、commit 或远程操作；工作树包含此前已保留的广泛重构。查看新增测试及截图的具体文件见[增量报告](native-cli-r2-contexts-2026-09-20.md)。本轮没有新的性能分位数测量，不能据此升级 R0 的性能声明。

试用当前能力可用[正式入口](../../README.md#从项目目录启动-v2-工作台)：右侧直接输入，查看中央用户/模型/工具区分，展开命令或拟议 Diff，再切换“网络 → 上下文与用量”。右侧无启用/释放按钮，只有另一页面正在输入时才显示“在此输入”。正式入口使用用户自行选择的真实配置；本报告中的自动化均为合成测试，不代替用户实际试用。

**下一步先等待用户试用反馈；收到继续指令后才开始 R3。** R3 最小切片是有界异步 journal、实际 fsync 持久水位与保存失败提示，不改动网络/PTY 热路径。
