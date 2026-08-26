# Codex Local Observer V1 Web Viewer 改进需求

## 1. 文档状态

- Status：P0/P1 + Conversation-first UX Implemented（P2 follow-ups retained）
- Date：2026-08-16
- Baseline branch：`codex/feat/project-context-web-viewer`
- Baseline commit：`3df995c`
- Delivery target：V1 本地 MVP 可用性加固

本文记录当前 Web Viewer 的已验证问题和下一步改进要求。它是
[`codex-local-observer-detailed-design.md`](codex-local-observer-detailed-design.md)
第 17 节的实施补充；若两者冲突，以详细设计中的安全边界和数据契约为准。

## 2. 目标与非目标

目标：让用户在桌面和窄屏环境中可靠地找到 Thread、理解捕获状态、阅读时间线并检查已脱敏诊断数据，同时确保页面不会把加载失败、实时连接限制或数据缺口伪装成健康或完整。

非目标：

- 不加入消息发送、approval、question、interrupt 等 V2 控制能力；
- 不扩展为远程、多用户或云端 Viewer；
- 不以品牌重设计、复杂动画或主题系统取代 MVP 可用性修复；
- 不展示未脱敏数据或 Codex 未公开的 hidden chain-of-thought；
- 不要求首批改进重写现有 Preact 技术栈。

## 3. 调研基线与证据

本次结论来自以下检查：

- 对照 Web Viewer 详细设计与 `web/src/App.tsx`、`web/src/style.css`、`web/src/api.ts`；
- 使用仓库合成 fixture 启动 Viewer，检查桌面列表、Thread 详情和 390px 窄屏布局；
- 验证搜索按钮和 Enter 键行为；
- 检查 bearer、Cookie 与 `/v1/stream` 的认证路径；
- 运行 `npm test` 和 `npm run build`。

已确认的基线结果：

- 前端 3 个 Markdown 安全测试通过；生产构建成功；
- 构建产物主 JS 约 1.04 MB，Vite 报告 chunk 大于 500 kB；
- 长 parent/fork relation key 会撑出详情区域，产生页面级横向滚动；
- 390px 下列表与详情上下堆叠，选择 Thread 后列表仍占据约半屏；
- 搜索输入框按 Enter 不触发搜索，点击按钮后仅按 Thread 过滤，不展示命中片段或查询状态；
- 页面用 bearer token 请求普通 API，但原生 `EventSource` 不能附加 Authorization header；Cookie 会话不存在时，SSE 与普通 API 的认证能力不一致；
- Thread 详情初始加载同时拉取全部 turns、items 和 raw events，即使 Raw Inspector 尚未展开；
- `coverageSummary`、`pendingRequests`、`projectionConflicts` 与 health privacy warning 已由后端提供，但页面未形成明确呈现。

### 3.1 实施结果（2026-08-22）

P0/P1 加固已在同一 Viewer 技术栈内完成：

- 820px 及以下切换为列表/详情单视图；长 ID、路径、Markdown、JSON 和 relation 均限制在自身容器；
- Cookie 模式使用 SSE 并独立显示连接状态，bearer 模式只使用轮询，后端 health 不再被传输错误覆盖；
- Dashboard、搜索、详情和 Raw Inspector 使用独立 loading/error 状态；Thread、搜索与 Raw 请求支持取消和过期响应隔离；
- 搜索支持 Enter、snippet、结果数、清除、筛选组合和精确 Turn/Item 定位；
- Thread 状态、resolved/unresolved relation、coverage、pending request、projection conflict、decode/unknown 与 privacy warning 已呈现；
- Timeline 使用 renderer registry 区分 message、tool、collaboration、media、request、status 与 unknown；
- Raw Inspector 首次展开后分页请求，展示 provenance 元数据并只复制已脱敏 JSON；
- 控件补齐可访问名称、live region、`aria-current`、`:focus-visible` 和窄屏 44px 交互目标；
- highlight.js 改为按语言注册，生产主 JS 从约 1.04 MB 降至约 153 KB，消除 500 kB chunk 告警；
- Vitest 增至 14 个测试，Playwright 新增 6 个 Chrome E2E，覆盖四档宽度、窄屏导航、搜索定位、Raw 按需加载和乱序响应。

仍保留的 P2 工作：完整 URL state/前进后退恢复，以及 10,000 Thread/Item 下的渐进列表或虚拟化性能基线。它们不阻塞本文第 7 节定义的 P0/P1 MVP 加固完成标准。

### 3.2 对话优先体验重构（2026-08-24）

- presentation registry 同时检查规范化 `itemType` 与原始 payload type，官方已知变体不再误报为未知；
- 用户和助手消息组成默认阅读流，其余过程按 Turn 汇总，相同 `call_id` 的调用与输出合并；
- 健康过程和真正 unknown 默认折叠，失败、pending 与 interrupt 自动展开，Raw/provenance 仍完整可查；
- 四个筛选器收进一个入口，source 级 decode/unknown/disconnected 只聚合提示一次，不再污染每个 Thread 行；
- Thread 诊断和完整性技术信息降级到折叠或异常提示，不占据健康 Thread 的默认阅读路径；
- `serve` 就绪后直接打印单次配对 URL；直接访问的登录页以配对链接为主，手工 bearer 仅保留为高级恢复入口。

本次仅改变前端 presentation 和启动入口，不修改 REST、SQLite、raw event 或 projection 契约，也不需要 migration。

## 4. 优先级定义

| 级别 | 含义 |
| --- | --- |
| P0 | 会阻止可靠使用、造成状态误导或破坏核心阅读路径，下一迭代优先修复 |
| P1 | 明显削弱 V1 的查找、理解和诊断价值，应在 MVP 加固阶段完成 |
| P2 | 规模、效率和体验优化，可在核心路径稳定后实施 |

## 5. 已确认改进需求

### WV-001：响应式导航与布局边界（P0，Implemented）

现状：长 Thread/relation 标识会造成详情页横向滚动；窄屏同时保留列表和详情，详情只能从页面下半部分开始阅读。

要求：

- 桌面保持双栏布局，任意 thread key、路径、代码块和 JSON 只能在自身容器换行或局部滚动；
- 小于或等于 820px 时采用列表视图与详情视图切换，选择 Thread 后详情占据主内容区域；
- 返回列表时恢复原有搜索、筛选、项目展开状态和滚动位置；
- 详情、Context、Timeline 和 Raw Inspector 不得产生页面级横向滚动。

验收：在 390、820、1280、1440px 宽度下使用超长 ID、路径和无空格文本检查页面；除代码/JSON 局部容器外，`documentElement.scrollWidth` 不得超过 viewport 宽度。

### WV-002：认证模式与实时状态一致（P0，Implemented）

现状：Cookie 会话可用于 `EventSource`，手工 bearer token 登录只能给 `fetch` 添加 Authorization；当前 UI 仍无条件创建 SSE。

要求：

- Cookie 会话继续使用 SSE，并在断开后显示“实时连接已断开，正在重试”；
- bearer 模式不得把 token 放入 URL、query 或日志；在没有 Cookie 时明确降级为定时轮询，不创建无法认证的 SSE；
- health、数据源健康和 Viewer 传输状态分开展示，SSE 失败不得把数据库健康状态直接改写为 degraded；
- 轮询成功后不得掩盖仍处于断开状态的实时连接。

验收：Cookie 与 bearer 两种登录分别验证；检查网络请求中不存在 token query；断开 SSE 后列表仍可轮询更新，页面同时保留正确的后端 health 与传输状态文案。

### WV-003：加载、错误与请求竞态（P0，Implemented）

现状：Dashboard、搜索和 Thread 详情缺少独立 loading 状态；快速切换 Thread 时旧请求可能覆盖新选择；详情失败后可能保留旧内容，错误提示不可关闭。

要求：

- 为认证、Dashboard、搜索、Thread 详情和 Raw Inspector 建立独立的 idle/loading/success/error 状态；
- Thread 切换时取消旧请求或使用 request identity 丢弃过期结果；
- 加载期间保留稳定布局并明确标记正在加载的目标，不把旧详情显示为新 Thread；
- 错误就近显示安全摘要，提供重试和关闭；401 清理 bearer 会话，其余错误不得自动清除已成功加载的数据。

验收：用延迟和乱序响应连续选择多个 Thread，最终详情必须始终匹配最后一次选择；分别覆盖 Dashboard、搜索、详情和 raw event 请求失败。

### WV-004：可解释、可定位的搜索（P1，Implemented）

现状：只有点击按钮可触发搜索；返回的 `SearchResult.snippet`、turnId 和 itemId 未使用。

要求：

- 搜索采用语义化 form，Enter 与点击触发相同行为；
- 显示当前查询、结果数、加载/失败/无结果状态，并提供清除入口；
- 结果列表展示后端返回的安全 snippet，不在客户端重新拼接未清洗 HTML；
- 点击结果打开对应 Thread，并定位或醒目标记对应 Turn/Item；
- 搜索与 source/status/completeness/archived 筛选的组合关系必须在 UI 中可见且可重置。

验收：覆盖 Enter、按钮、清空、零结果、特殊字符、分页超过 200 条以及筛选组合；结果定位后不能误跳到同 Thread 的其他 Item。

### WV-005：Thread 状态与关系可读（P1，Implemented）

现状：列表主要展示 completeness 和 archived，未充分表达 stale、source disconnected、unknown/decode、sub-agent、parent/fork；详情关系常退化为长内部 key。

要求：

- 状态使用文字和图形/图标共同表达，不只依赖颜色；
- 列表至少区分 active、stale、archived、capture completeness、source disconnected 和 sub-agent；
- unknown/decode 风险在可获得诊断摘要时显示，不因缺少列表字段伪造为零；
- resolved parent、fork 和 child relation 可点击导航；未解析关系显示短 ID、关系类型和 unresolved 文案；
- completeness badge 必须配套真实 reasons，不以固定成功文案替代未知原因。

验收：合成 fixture 覆盖 durable/live complete/partial、metadata_only、ephemeral_lost、stale、disconnected、archived、sub-agent、resolved/unresolved relation 和 unknown/decode。

### WV-006：按 Item 类型呈现 Timeline（P1，Implemented）

现状：大部分 Item 共用“类型标题 + summary + raw”卡片，工具参数、结果、文件变化、时间和请求状态不易辨认。

要求：

- 建立可扩展 renderer registry，至少覆盖详细设计 17.3 中列出的 Item 类型；
- user/agent message 保持安全 Markdown；command、file change、MCP、collab、web search、image 和错误使用各自的摘要结构；
- Turn 和 Item 显示可用的时间、持续时间、phase、status 和 completeness；
- approval/user question 只显示来源、时间、pending/resolved/disconnected 状态和固定只读提示，不出现可提交控件；
- unknown renderer 始终展示 method、phase、size、时间和安全 JSON tree。

验收：每个 renderer 至少一个正常 fixture；unknown 字段、空 payload、超大 payload、失败和中断不能导致整条 Timeline 崩溃。

### WV-007：诊断摘要与按需 Raw Inspector（P1，Implemented）

现状：详情一次性获取并在单个 `pre` 中组合全部 diagnostics 和 events，已有 coverage、pending、conflict、privacy 数据未被解释。

要求：

- Thread 顶部提供 coverage、pending request、projection conflict、decode/unknown 和 privacy warning 摘要；
- Raw Inspector 默认折叠，首次展开后再按现有 event sequence 分页加载；
- 每个 raw event 展示 source、epoch、sourceSeq、eventSeq、method、phase、durability、decodeStatus、redaction、hash 和时间；
- 提供“复制已脱敏 JSON”，不得提供未脱敏数据入口；大 payload 继续通过受保护 blob 下载；
- JSON、ANSI、HTML 和 SVG 始终作为不可信纯文本处理。

验收：展开前不得请求 raw event 列表；分页、失败、重试、复制、blob 引用、恶意 HTML/SVG/ANSI 和 legacy redaction warning 均有测试。

### WV-008：可访问性与键盘操作（P1，Implemented）

要求：

- 所有筛选控件有独立可访问名称；button、select、input、summary 和结果项都有清晰 `:focus-visible`；
- 搜索结果数、错误和连接状态使用适当的 live region，但周期刷新不得重复播报无变化状态；
- 项目树与 Thread 列表支持合理的 Tab 顺序，选中项使用 `aria-current` 或等价状态；
- badge、图标和颜色均有文字替代；交互目标在窄屏满足至少 44px 的可点击高度。

验收：只用键盘完成登录、搜索、筛选、选择 Thread、展开 Context/Raw 和返回列表；进行基础语义与对比度检查。

### WV-009：页面状态与大数据效率（P2，Partially Implemented）

要求：

- 将选中 Thread、查询和筛选同步到可安全分享的 URL state；刷新和前进/后退可恢复页面；token 不得进入 URL；
- Thread 列表采用服务端分页、渐进加载或虚拟化，不一次渲染全部展开项目；
- turns/items 保持分页语义，raw events 按需加载；
- 评估 highlight.js 按语言注册或动态加载，消除无理由的主 chunk 体积告警；
- 性能优化不能静默截断结果，必须显示已加载范围与继续加载状态。

验收：10,000 Thread 和 10,000 Item 场景下完成列表首屏、搜索、Thread 打开和 Timeline 滚动冒烟；记录可复现的构建体积与交互时间基线。

### WV-010：前端 fixture 与自动化质量门槛（P0，Implemented for P0/P1 paths）

现状：前端仅有 Markdown 渲染安全测试，现有 fixture 主要覆盖 durable complete 正常路径。

要求：

- 增加组件测试：搜索提交、筛选组合、状态 badge、relation、renderer、loading/error 和请求竞态；
- 增加响应式 E2E：桌面双栏、窄屏列表/详情切换、无页面级横向滚动；
- 增加安全回归：Markdown/HTML/SVG/ANSI、外部/本地链接、Raw Inspector 和复制内容；
- 扩充合成 fixture，覆盖 WV-005、WV-006、WV-007 所需状态，不读取真实 `~/.codex`；
- 保持 `npm test`、`npm run build`、Rust API 契约测试和相关 E2E 全部通过。

## 6. 建议实施切片

1. **可用性止血**：WV-001、WV-002、WV-003、WV-010 的对应回归测试；
2. **查找与导航**：WV-004、WV-005；
3. **时间线与诊断**：WV-006、WV-007；
4. **可访问性与规模加固**：WV-008、WV-009。

每个切片应保持 Viewer 可运行，并在同一 PR 更新实现、测试、本文状态和详细设计中的相关契约。若 Thread 列表需要新增诊断摘要字段，只允许 additive API 变化，并同步 API 契约测试；搜索定位优先复用现有 `SearchResult`，Raw Inspector 优先复用现有 event sequence 分页。

## 7. 完成标准

本改进方向完成需同时满足：

- P0、P1 需求全部达到各自验收条件；
- 桌面和窄屏核心路径可通过键盘完成；
- health、传输状态和 capture completeness 不互相冒充；
- 不引入 V2 mutation API，不削弱 loopback、认证、CSP、redaction 和只读边界；
- 自动化测试、构建、类型检查及相关 Rust 测试通过；
- 详细设计、README 能力描述与实际实现保持一致。
