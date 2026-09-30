# 合成场景与专项调试

[开发指南](README.md) · [验证报告](../validation/README.md)

以下历史调试命令用于显式专项检查。先通过标准入口构建；真实 CLI 子进程必须隔离 HOME/USERPROFILE 和 CODEX_HOME，不能指向私人数据。现有 `/private/tmp` 示例是 macOS 临时路径，复现时选择自己的私有临时目录；未登记输出不会被自动过期删除。

## 查看合成阶段调试

当前阶段已接上三列布局、受认证 WebSocket、原生 xterm、右侧“用户”气泡与左侧“模型”流式卡片，并在对话中交错呈现独立工具卡。用户提交来自明确匹配的原生记录，模型名有来源依据；HTTP 辅助/未知请求可在“用量概览 → 查看调用记录”中选中并展开。工具参数生成与执行状态分开，命令输出、退出码与耗时仅在存在关联证据时显示；Code Mode 保留代码工具卡。正式入口现已接入项目文件树、代码搜索及 Git 只读阅读；磁盘历史也使用上面的正式入口。下面保留的 R1 演示入口未安装 Recorder，使用普通安装版 CLI、临时项目/`CODEX_HOME` 和本机合成模型，只演示文字聊天，不访问真实模型：

```bash
npm run build --prefix web
cargo run --locked --offline --example r1-debug -- --state-file /private/tmp/codex-r1-debug.json
```

打开状态文件中的配对 `url`，即可直接在右侧完成官方 CLI 的主题/信任初始化并输入，无需启用或释放输入权；此模式不自动提交任务。中央展示合成模型的真实分片流。终端开关、调用详情打开/关闭和同进程刷新可直接操作；只有另一页面正在使用终端时才显示“在此输入”，点击一次即可切换，原页面随后只读。输出始终可读。正式用户配置和旧服务不变。

使用已安装 Chrome 自动检查同一条路径：

```bash
cargo run --locked --offline --example r1-debug -- \
  --state-file /private/tmp/codex-r1-probe.json --probe \
  --screenshot /private/tmp/codex-r1-probe.png
```

探针在隔离 CLI 内提交两次相同的中文多行文本，核对两组用户/模型消息、草稿排除、正文中间态、模型身份、辅助/未知隔离、上滚/跟随、同进程刷新、双页接管、窄屏和原生退出。用户 reader 在探针中刻意延迟 4 秒，验证迟到气泡不抢滚动锚点或终端焦点；正常调试没有此延迟。测试不下载浏览器；可用 `WORKBENCH_TEST_CODEX` / `WORKBENCH_TEST_CHROME` 指定已安装的可执行文件。证据和剩余范围见[聊天记录](../validation/native-cli-r1-user-chat-2026-09-19.md)。

R0 最小正文阅读入口也保留：

```bash
cargo run --locked --offline --example r0-debug -- --state-file /private/tmp/codex-r0-debug.json
```

工具卡可通过[实际 CLI/Chrome 验证](../validation/native-cli-r2-tools-2026-09-19.md)查看截图或复现。此测试由合成模型驱动安装版 CLI，在临时项目中真实读取、修改文件及运行退出码 7 的命令，分别覆盖 Code Mode 与直接命令模式；不会向现有用户调试会话提交任务：

```bash
WORKBENCH_TEST_SCREENSHOT=/private/tmp/codex-r2-tools-20260919 \
  cargo test --locked --offline --lib \
  workbench::proxy::tests::native_cli::tools_browser -- --ignored --nocapture
```

原生命令迟到退出、真实 `write_stdin` 轮询及长输出的验证见[增量报告](../validation/native-cli-r2-native-results-2026-09-19.md)。可用同样的临时环境测试，命令为 `cargo test --locked --offline --lib workbench::proxy::tests::native_cli::rollout_tools -- --ignored --nocapture`；结果分别标明模型请求或原生记录来源，输出预览上限 64KiB，完整日志未保存在工作台。

独立“模型请求/网络”页面已取消：模型回复和工具卡旁的“调用详情”打开中央侧面板；左下角“用量概览 → 查看调用记录”提供模型、用途、逐次 Token、状态与观察时长列表。辅助和未归属正文只在对应详情折叠展示，避免复制聊天。历史记录提供自己的调用入口，底栏入口始终属于当前运行。详情只在打开、分页或手动刷新时读取。系统说明、历史输入、工具 schema/custom format 与响应各自有来源；历史上下文不会新增用户气泡。接收新响应后提示刷新，分页过期不会混入其他版本；用量按响应列出，不将缺失视为零，也不汇总成任务总量。实时详情缓存只属于当前运行，R3 正式入口另有异步保存；历史详情通过历史 API 阅读。超限、淘汰或未保存内容不能冒充完整历史。当前入口与截图见[调用阅读验收](../validation/native-cli-call-inspection-2026-09-20.md)，底层详情契约另见[原请求详情验证](../validation/native-cli-r2-request-details-2026-09-19.md)。可在构建后运行合成 Chrome 场景：

```bash
WORKBENCH_TEST_SCREENSHOT=/private/tmp/codex-r2-request-details \
  cargo test --locked --offline --lib \
  browser_reads_context_on_demand_without_losing_terminal_or_reading_state -- --ignored --nocapture
```

两个入口的 `--state-file` 都必须是尚不存在的本地文件；本次浏览器配对 `url` 只写入权限 0600 的该文件，不输出到通用日志。R0 自动提交一次合成任务，R1 等待终端原生输入；刷新均不会再次提交。Ctrl-C 结束持续调试并清理临时目录和配对文件，内存阅读记录不保存。验证失败时残留的状态文件不表示服务仍在运行。

[三列交互原型](../prototypes/native-cli-workbench.html)可独立打开对照，其中的终端、文件、工具与 Git 内容为合成演示，不代表当前运行时已经接入。

## 历史兼容与入口回归

R6 当前 CLI/profile、历史与设备范围见 [G3 验证](../validation/native-cli-r6-g3-compatibility-2026-09-29.md)。先运行 `node scripts/dev.mjs build --offline`，再显式执行安装版 CLI 或合成浏览器矩阵；它们内部为真实 CLI 子进程隔离 HOME/USERPROFILE/CODEX_HOME，模型使用本机合成上游：

```sh
cargo test --locked --offline --lib workbench::proxy::tests::native_cli:: -- --ignored --nocapture --test-threads=1
cargo test --locked --offline --lib workbench::web::tests:: -- --ignored --nocapture --test-threads=1
```

新诊断应按[受管产物流程](README.md#构建产物生命周期)登记。`WORKBENCH_TEST_SCREENSHOT` 使用对应任务目录中的截图前缀；测试不会下载 Chrome。非空 `history_base` 的 fork 恢复按当前契约拒绝，测试分别验证实时 fork、拒绝后历史可读及原会话显式恢复，不能把这项负向通过描述为支持继承链恢复。

实际旧库只读抽查独立于合成矩阵，必须由执行者显式选择已经存在的来源和预期 schema；下面路径只是占位示例：

```sh
WORKBENCH_TEST_LEGACY_DB=/absolute/path/to/observer.sqlite \
WORKBENCH_TEST_LEGACY_SCHEMA=25 \
cargo test --locked --offline --test legacy_readonly_probe -- --include-ignored --nocapture --test-threads=1
```

该探针使用生产只读 reader，读取有界页及前十个会话条目，前后比较源文件/辅助文件完整 hash 与写入元数据；仅打印聚合计数，不输出正文、ID、路径、游标或 hash。它不会 import、migration、修复或复制来源，需具备只读权限；不能用一次限定抽查声称全库完整，也不应将私人来源接入日常自动化。
