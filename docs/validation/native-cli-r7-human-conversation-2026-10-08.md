# R7-E：任务识别与人类对话呈现验证

日期：2026-10-08。基线 `b0ae129`，本地分支 `codex/session-project-clarity`；包含此前 R7-A–D 未提交改动。本报告是用户试用后确认四项体验改造的增量证据，不以旧 R7 通过推导本轮体验通过。契约见[整理方案 §10](../codex-native-cli-workbench-session-organization.md#10-试用改进任务识别与人类对话优先)、[ADR 0056](../decisions/0056-human-conversation-presentation.md)；阶段状态仅在[实施计划](../v2-implementation-plan.md#r7-session-organization)维护。

## 1. 改造范围

1. 来源独立的有效原标题优先，否则第一条真实用户请求；只有已确认完整包装可排除。标题最多120个Unicode字符，无可靠请求时保留未命名。
2. 正文默认突出对话；上下文、工具、内部和未知记录按需展开，错误/缺口保持可见，原始脱敏详情仍可核查。首个对话自动定位最多8窗口/128条，搜索命中窗口自动展开。
3. 列表突出任务、当前来源已观察的交流预览和时间。预览最多240个Unicode字符，界面安全转为纯文本、优先叙述，保留行内标识符与代码-only回退；不生成总结、不借另一来源文本、不推断完成状态。
4. 选中项目作为h1与标签页标题，来源、完整路径、精确计数和关联来源操作移至次级入口；辅助/运行按钮不重复技术计数。

空白第五项不扩展。没有项目合并、worktree、批量重归类、原始历史清理、模型主题生成、控制入口或权限扩大。catalog派生投影3→4重建；organization schema1、JSON配置schema2不变，恢复线程与cwd不变。

## 2. 自动化与构建

环境：macOS Apple Silicon，Rust1.95.0、Node22.23.2，系统已安装Chrome。自动化使用合成来源与隔离HOME/USERPROFILE/CODEX_HOME，不下载浏览器，不写真实原生目录。

| 检查 | 实际结果与证据 |
| --- | --- |
| 标准 `node scripts/dev.mjs check --offline` | 受管任务 `f8719a96-eedf-4dbe-9858-c115fddd0875` 成功；维护工具80、Web285、Rust all-targets488 passed/59 ignored，fmt、all-target Clippy、debug/release两binary构建通过 |
| 其后界面精修及完成审计 | 当前代码重新执行受管任务 `7ce74e85-02b1-4617-b931-1eb2a8a1bc86`：全部Web288 passed（35文件），TypeScript通过；Rust运行实现没有晚于最终标准check的修改，仅浏览器fixture随后更新并通过最终专项 |
| 历史模块专项 | 80 passed/11 ignored：完整AGENTS/封闭嵌套环境、混合请求、畸形/引用/未知XML保守保留，三来源呈现、Observer lastEventSeq顺序、脱敏/Unicode限长、v3重建/归属保留和源零写 |
| 标准浏览器 `node scripts/dev.mjs e2e` | 10 passed，系统Chrome；受管任务 `8c48c18b-63ba-4000-a201-b91a95dbbb2c`。这是既有兼容页面回归，不能单独作为本轮工作台体验证据 |
| 正式binary三来源Chrome专项 | 最终 `680162fd-9544-4666-9cae-faaea0c3a02a`：1 passed/0 failed/439 filtered，19.98s；包含真实请求标题搜索、320px实际文本宽度和完整短标题回归 |
| 最终release重建 | `9c9af0a9-5a11-418f-a6ab-d1acf5d0952d` 成功，先Web166 modules再两个Rust binary；窄屏与正文最终精修全部嵌入，重启后HTTP200、三个入口资源与Web产物逐字节一致 |

暂存区二进制检查0文件/0问题，不冒充全部增量文件审查；54个已改/新增文件按仓库内容分类器检查，0禁止二进制；有限secret格式检查0匹配文件，不输出匹配内容、不代替人工隐私审查。新增文件均为文本，fixture为合成；最终Rust fmt通过，文档111文件/0问题、diff whitespace通过。

## 3. 正式页面增量验收

合成7条来源记录：native主会话含48条完整长AGENTS/环境前缀和混合真实请求，错误、缺口、未知过程；child含144条前缀；Observer有有效已存标题；Workbench只有助手交流，项目名不冒充任务标题。使用正式debug binary内嵌页面，不启动CLI；原生主/子、未归类来源、Observer数据库与Run元数据进行字节零写核验。

完整专项已通过：首屏真实请求及AGENTS排除、独立preview与Observer标题、真实请求标题元数据搜索、错误/缺口可见、unknown展开、混合原始详情/返回焦点、128条有界提示、上下文/工具搜索展开、单条归属保存/撤销、URL/焦点/重新载入位置、3窗口最多48记录DOM，以及1440/1024/736/390/320px真实几何。最终受管任务 `680162fd-9544-4666-9cae-faaea0c3a02a`，19.98s；0 WebSocket、0 page error、0外部请求、0 CLI启动，原始数据字节零写。320px正文实际宽205px，标题宽/scrollWidth均205px，高/scrollHeight均21px；完整显示短标题，时间独立下一行。模式切换漂移0.328125px/0px，仍严格满足<2px。主代理直接查看该目录的项目1440、736、320与请求首屏四张实际截图；截图仅为合成验收页面，保留在受管产物中，不作为私人截图提交。

### 修复与失败证据

- 首轮标准check的本机监听被沙箱拒绝（`b6055fed-ee6b-4ff6-842b-1a05730f6320`），不是通过；在隔离测试环境申请本机监听后复跑。
- 新增fixture的child路径被进程handle遮蔽，编译失败（`9bf11d2e-fdcd-47ba-87fa-b843ae18f969`）；改名child_path后完整标准check通过。
- 旧浏览器断言曾依赖技术计数、旧提示容器和Workbench项目名标题；按确认的新契约更新，不放宽真实行为门槛。
- 实际截图暴露preview保留Markdown/HTML/代码标记；新增共享安全纯文本预览及回归，原DTO/raw不变。
- 已确认上下文曾误标“未识别记录”；改为明确会话上下文，未知过程仍可辨认。
- 切换全部记录发生8.125px漂移（`69625a32-5900-4913-90d3-4b354059d828`）。虚拟窗口wrapper未包含折叠孩子的外边距；`display:flow-root`使测量边界包含margin。复验 `7e7c13bb-b593-4c96-ac90-2aa9ce5914d1`：443.09375→442.96875→443.09375，分别漂移0.125和0px，仍使用<2px门槛。
- 长前缀使随后工具与模型回复落在既有64KiB搜索摘录预算之外，搜索无结果（`7e7c13bb-b593-4c96-ac90-2aa9ce5914d1`及后续复验）。初步只索引工具名的解释不准确；源码确认摘录取完整record body（含raw），但仍有总量边界。fixture在预算内放入明确工具用于真实API搜索展开验收，不增加预算或宣称全文可搜；标题元数据的真实请求搜索另行核验。
- 主代理查看320px截图发现时间挤占正文，短标题也截断。改为窄屏icon/正文两列、时间位于正文下方，标题最多两行；增加实际文本宽度与完整短标题验收，不只检查document无横向溢出。

## 4. 试用与限制

最终试用实例更换前，鉴权只读核验旧实例`34a69046-130f-4af2-92dc-634d7d1fdb3a`的runs为空，正常结束本任务启动的旧服务。当前新版实例`fe37f172-63cd-4058-95d3-d036938500f7`，本机地址`http://127.0.0.1:60777`；无参数`codex-view --no-open`仅启动历史服务，新实例runs为空，HTTP200，三个入口资源与当前Web产物逐字节一致。随后`codex-view open <entryFile>`通过私有入口调用系统浏览器，命令exit0；这仅证明打开已派发，不冒充用户页面已完成鉴权/人工浏览。没有启动CLI或操作用户原始会话/真实归属，不输出pair secret或Cookie。

此前用户Chrome手动导航出现`ERR_BLOCKED_BY_CLIENT`；服务HTTP200正常，不绕过客户端屏蔽或改变浏览器安全/代理设置。本轮浏览器验收采用隔离的系统Chrome合成页面及实际截图，不能宣称用户Chrome配置已修复。

当前试用项目只读API抽查11条来源记录，全部有独立交流preview，AGENTS注入标头标题0条，未输出私人正文。该有限项目统计不能作为全部用户历史或未知包装识别保证。

未知/畸形注入形状仍保守显示；有限preview仅代表当前来源已观察范围，Observer/Workbench未读取尾部不保证最新。搜索仍受既有索引与摘录预算限制。R7-E没有重新跑所有ignored真实CLI、10k资源或手机专项；原恢复路径相关常规测试继续通过，不据此新增CLI/provider/platform兼容承诺。

改动未提交、推送、创建Issue/PR或发布。下一步是用户继续体验任务识别与项目分组；真实归属调整仍需用户明确操作。

## 5. 完成审计

逐项以当前源码、真实测试日志、已查看的渲染截图和运行资源核验，不以本报告或计划的Passed文字作为通过依据。前一轮完成生产改造、浏览器复验、release重建与安全重启，属于有实质进展；本轮补充当前前端复跑与运行资源核验，不重启或扩大范围。

| 明确要求 | 当前实现与直接证据 | 结论 |
| --- | --- | --- |
| 1. 有效原标题优先，真实请求回退，排除已确认注入 | `presentation.rs`的stored_title/observe/without_context与Observer name入口；标准check日志确认标题优先、封闭/混合/畸形/引用/嵌套环境回归通过，三来源fixture含既有Observer标题及native真实请求；最终Chrome的humanTitleSearch和humanRequestFirstScreen实际断言通过 | 已实现并验证，不对未知包装做激进删除 |
| 2. 人类对话默认突出，技术过程按需，原记录保留 | `LibraryReader.tsx`默认allRecords=false，`HistoryRecord.tsx`按presentation折叠并保持attention可见；当前7项reader测试通过。正式binary专项核验展开、详情、搜索、128条边界、锚点/刷新，并逐字节比较native主/子/未归类、Observer库与Run meta；混合消息原详情仍包含上下文 | 已实现并验证，展示投影不改源记录 |
| 3. 标题/最近有效交流片段/时间优先，来源路径诊断次级 | `SessionGroupList.tsx`及全部来源行采用独立ExchangePreview与time，技术信息位于默认关闭的details；当前8项组列表与3项安全纯文本预览测试通过。最终Chrome独立预览断言及1440/736/320截图，320实际正文205px、短标题未截断、时间下置 | 已实现并验证，预览只代表当前来源已观察交流，不生成任务状态 |
| 4. 当前项目主标题，减少重复按钮/计数 | `HistoryLibraryPanel.tsx`稳定selectedProject决定h1与document.title；路径和精确计数按需，辅助/运行按钮无重复技术计数，进入工作台/开始新对话互斥。当前45项面板测试通过，正式Chrome在项目不在导航页、来源过滤/无结果时仍核验项目主标题；实际项目截图已查看 | 已实现并验证 |
| 构建、文档、安全与可试用交付 | 标准check与最终release任务成功；当前Web288/typecheck复跑、文档111/0、diff whitespace和54个增量文件内容检查通过。完成审计再次只读GET：HTTP200，三个入口资源与当前产物逐字节一致；未启动CLI、改真实归属或执行远端mutation | 实施与验证目标完成；手动Chrome客户端屏蔽、有限搜索及未重跑的非本轮专项仍按§4披露 |

没有剩余的本轮必做实施或验收项。用户继续试用是交付后的反馈步骤，不以合成浏览器结果代替用户人工认可，也不把其尚未反馈解释为实现阻塞。
