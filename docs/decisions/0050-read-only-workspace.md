# ADR 0050：固定项目根的文件、搜索与 Git 阅读

- Status：Accepted
- Date：2026-09-20

## Context

R4 接入当前项目文件、搜索和 Git，网页同时能看到不可信工具参数、文件名与内容。仅检查路径字符串后让 rg/Git 再打开原路径，无法防止校验后的符号链接替换，也可能启动仓库配置的外部程序。此能力必须与转发、PTY 和记录器隔离，不能让浏览器选择新的根、程序或环境。

## Decision

Launcher 将 canonical cwd 的只读目录描述符交给独立 WorkspaceReader 线程。最多排队 8 个请求，每个请求含排队在内 5 秒预算。HTTP 取消时置位取消标记；队列满立即拒绝。文件 I/O、rg/Git 的启动与读取都在独立线程，网络/PTY/记录器不引用其队列。已进入内核的磁盘 I/O 无法硬取消，超时后网页结束等待，后续请求仍受有界队列约束。

文件逐组件 `openat(O_NOFOLLOW)`，中间组件要求目录，末尾要求单硬链接普通文件，拒绝所有符号链接（包括指向根内的链接）。根本身被重命名后仍绑定原目录身份。拒绝绝对路径、`.`/`..`、空组件、反斜杠、NUL、超过 64 层或 4096 字节路径。常见凭证目录/文件名不进入列表或正文，包括 `.git`、`.ssh`、`.aws`、`.azure`、`.kube`、`.gnupg`、`.docker`、`.codex`、`.codex-web`、`.env*`、`auth.json`、`credentials*`/`secrets.*`、私钥名及 `.pem/.key/.p12/.pfx/.jks/.keystore`。规则以代码为准；它不是任意源码中嵌入秘密的自动识别器。

目录每页 200 项，至多扫描 10,000 项/1 MiB 文件名；游标绑定目录路径及已读取条目的摘要，集合变化返回 409。文件最大 1 MiB，UTF-8/NUL 检测拒绝二进制，读取前后检查元数据变化。前端原先每段 200 行、每行 4000 字符的展示限制已由 [ADR 0052](0052-continuous-file-reading.md) 替代：同一文档连续滚动、视口虚拟渲染、长行横向阅读；后端 1 MiB 上限不变。

搜索先用固定 `rg --files --hidden --no-ignore-global --no-ignore-parent --no-config --null -- .` 枚举当前目录，遵守项目 ignore 规则。安全打开并校验文本后，以最多 16 个匿名临时文件描述符为一批交给 `rg --json -e <pattern>`。rg 不直接重新打开项目正文路径，不启用 shell、preprocess、PCRE2 或压缩搜索。最多 200 处匹配、2048 文件/32 MiB 扫描和 5 秒；结果注明截断原因与省略数量。文件名含空格、中文、换行、前导 `-` 都不经过 shell 或逐行路径拆分。

Git 使用固定 cwd/argv 和清理后的环境，禁用 pager、外部 Diff、textconv、fsmonitor、hooks、可选锁、子模块递归、签名展示、对象替换及延迟抓取，禁止传输协议。status 使用 porcelain `-z`，仅处理当前 cwd pathspec，并按 Git prefix 再过滤一次；log 按当前目录过滤且每页 20 条，游标绑定 HEAD 与 prefix，不要求配置 upstream。缺少 Git/rg 分别报告能力不可用。

选中文件的 Diff 是文件内容对比：已暂存为 `HEAD → index`，未暂存为 `index → 安全读取的当前文件`。Git `ls-tree/ls-files -z` 定位 blob，`cat-file` 按验证过的 object ID 取内容；Git 的 `--no-index --no-ext-diff --no-textconv` 仅对比匿名文件描述符。这样不会让 Git 重新打开可能被替换的工作区路径。文件模式与 rename 仍在 status 展示，内容 Diff 不冒充完整可应用的 Git patch；未解决冲突、链接、二进制和超大文件明确不可预览。

子进程输出有界（stdout 1 MiB / stderr 丢弃最多 64 KiB），独立进程组在取消/超时/超限时结束并回收。stderr 不返回正文、路径或环境。匿名临时文件在关闭时消失，不形成新的历史/归档目录，也不写原生 Codex 数据。

左侧文件树/搜索/Git 与中央阅读分别维护选择。文件树按需展开，仅最后选中的目录自动刷新；其他展开目录保留读取结果。当前可见目录、Git 面板与中央文件每次完成后至少等待 5 秒再刷新，隐藏页面暂停，切换取消未完成请求。外部变化和已确认工具完成由下一次有界刷新反映，不把通知或工具完成当作 Git 状态凭证；首版不增加递归 watcher。点文件/搜索命中/结构化工具路径打开中央只读文件，关闭返回此前对话或历史。窄屏在选文件后收起左侧浮层。TerminalPanel 始终挂载，阅读动作不提交对话或重放终端输入；原生开启焦点报告时 xterm 继续发送焦点控制序列。

## Alternatives

- 直接将浏览器路径交给 rg/Git：更短，但重新打开存在替换竞态，并使外部 Diff/配置扩展成为执行入口。
- 自行实现正则和 Diff：增加两套语义与依赖维护，偏离使用已安装工具的最小范围。
- 复制整个项目或递归 watcher：增加数据副本、清理与高扇出刷新成本。本片只生成有界、短期、匿名正文快照。
- 允许安全根内 symlink：需要更复杂的解析与竞态策略，当前先明确拒绝。

## Consequences

本片不增加用户可调配置、migration、网页文件/Git 写接口或远程访问。查询不是文件系统/仓库全局事务，多次读取之间可发生变化；界面显示读取时间，旧结果不等于实时最新。文件名规则不保证任意文件正文无秘密。当前在 macOS、安装版 Git/rg/Chrome 验证，Linux/Windows 完整运行支持不因此扩大。实现验收及限制见 [R4 记录](../validation/native-cli-r4-acceptance-2026-09-20.md)，切片状态只见[实施计划](../v2-implementation-plan.md)。
