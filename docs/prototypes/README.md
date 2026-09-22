# 原生 CLI 工作台交互原型

本目录保存 2026-09-18 按用户提供的 cc-viewer 深色界面参照修订的设计原型。产品要求见[方案 §1.1–1.4](../codex-native-cli-workbench.md#ui-prototype)，生命周期契约见[详细设计 §6.2](../codex-native-cli-workbench-detailed-design.md#62-前端阅读状态与终端生命周期)，逐项操作与切片验收见[实施计划 §4.1](../v2-implementation-plan.md#prototype-acceptance)。

2026-09-19 终端交互已在实际工作台简化：单页打开即可输入，无启用/释放按钮；仅另一页面正在使用终端时显示“在此输入”，点击即切换，无确认弹窗。本目录 HTML/PNG 保留原始设计记录，其中接管确认演示不再作为实现要求；当前规则与截图以[实施计划](../v2-implementation-plan.md#prototype-acceptance)和[验证记录](../validation/native-cli-terminal-auto-input-2026-09-19.md)为准。

原型已演示用户气泡、模型正文与工具展开，但没有覆盖真实用户提取、流式角色、命令执行状态及并发结果关联。补充规则以[聊天角色与内容类型](../codex-native-cli-workbench.md#chat-content-design)、[类型化契约](../codex-native-cli-workbench-detailed-design.md#chat-item-contract)和[C01–C10 验收](../v2-implementation-plan.md#chat-acceptance)为准；这些是待实现要求，本次补写规则没有重新生成 HTML/PNG。

2026-09-20 用户进一步确认取消产品的独立“模型请求/网络”页面，入口改为模型/工具卡“调用详情”和用量概览“查看调用记录”，历史页提供自己的调用入口。此目录旧 HTML/PNG 的“网络”导航不再作为产品要求；当前交互见[方案 §1.6](../codex-native-cli-workbench.md#16-对话内调用详情与用量中的调用记录)。

2026-09-20 R4 已接入当前项目文件树、代码搜索和 Git 阅读。交互沿用左侧导航独立于中央阅读、打开文件才切换中央、右侧终端保持的约定；实际安全限制、行号定位、内容 Diff 与窄屏结果见 [R4 验收](../validation/native-cli-r4-acceptance-2026-09-20.md)。旧合成原型不替代这些运行证据。

## 文件与预览

| 文件 | 用途 |
| --- | --- |
| [native-cli-workbench.html](native-cli-workbench.html) | 可直接在浏览器打开的独立预览，GitHub 文件页需先下载 |
| [native-cli-workbench.fragment.html](native-cli-workbench.fragment.html) | 可维护的原型源文件；HTML、CSS、示例数据和页面内交互 |
| [native-cli-workbench.png](native-cli-workbench.png) | 1024px 桌面默认布局的静态对照图，由合成原型生成 |

预览不需要启动本项目服务、Codex CLI 或配置模型凭证。文件、消息、模型名、请求编号、时间、提交号和用量都是示例；文本框仅保存本页演示草稿，刷新即丢失。没有执行命令、写入项目文件、请求模型或调用控制 API 的能力。

可以点击：左侧文件/Git 导航、文件名、“返回对话”、顶部“网络”与“终端”、工具详情、拟议 Diff、搜索结果、历史记录，以及底栏状态。切换阅读面板或隐藏终端时，终端的演示草稿保持。2026-09-20 用户试用后，产品的底栏已改为 Token 用量优先、技术诊断折叠；此早期合成原型的底栏不再是该区域的最新设计，见[方案 §1.5](../codex-native-cli-workbench.md#15-用量概览与用户状态)。

独立预览保留正常状态；原始对话内原型另通过宿主设计控件提供终端位置/占比、保存异常、捕获缺口和只读接管的演示。这些可选宿主控件不在独立预览中显示，对应生产行为已写入方案。原型没有真实增量生成、滚动跟随、xterm/PTY、磁盘保存或网络重连。

预览的图标和悬浮提示依赖导出模板中固定版本的 CDN 静态库；内容与页面交互使用内嵌合成数据。离线阅读可直接使用 PNG 和方案中的交互表，不把图标加载成功视为后端连接成功。

## 更新约定

1. 编辑 fragment 源文件，并同步更新方案的交互/状态约定。不要只改独立预览内转义后的 HTML。
2. 使用可用的 visualize 技能 `scripts/render.py` 将 fragment 导出为独立 HTML；导出命令形式为 `python3 <visualize-skill>/scripts/render.py <fragment> <html> --force --title <title>`。导出器是文档维护工具，不是产品运行依赖。
3. 在系统 Chrome 中打开独立 HTML，确认图标完成加载，保存默认 Git 侧栏 + 中央对话 + 右侧终端的截图。截图只使用合成内容。
4. 检查面板切换、工具展开、请求详情、搜索跳转、终端草稿保留及 1024 / 736 / 320px 宽度，再同步链接和预览图。

首次归档已检查上述浏览器交互与宽度。保存异常、观察缺口和确认接管的纯页面状态在对话内原型中也已演示验证；这些检查均不替代 R0–R5 的真实功能、安全和性能验收。
