# R6-E：目录选择器启动与响应改进

日期：2026-09-23。用户反馈中文已生效，但窗口刚出现时需等待才能操作。本次仅调整 macOS 原生选择器，不进入 R6-F。

## 1. 观察与结论边界

对旧实现的临时诊断副本添加单调时钟标记和事件循环 timer。首次进程启动后约 0.500 秒创建面板，约 9.547 秒才收到窗口成为 key 的通知；期间 timer 正常执行。随后同一应用路径和新临时应用路径分别约 0.512、0.547 秒成为 key，线程采样显示正常的模态事件等待。

因此观察到的是一次初始窗口/输入焦点就绪延迟，没有证据说明工作台正在扫描目录或 helper 主线程持续死锁。也不能归因为每次新建临时应用包必然慢。首次慢启动尚未稳定复现，系统服务冷启动与应用激活时序尚不能完全分离。

Apple 的 `run` 负责应用启动及事件循环，`applicationDidFinishLaunching` 通知先于第一个事件；`NSOpenPanel` 的同步和异步 API 都合法，不能把 `runModal` 本身称为错误。[NSApplication.run](https://developer.apple.com/documentation/appkit/nsapplication/run())、[finishLaunching](https://developer.apple.com/documentation/appkit/nsapplication/finishlaunching())、[NSOpenPanel](https://developer.apple.com/documentation/appkit/nsopenpanel)。

## 2. 实现

- 使用 `NSApplication.run` 和强引用 delegate 管理启动生命周期。
- 在启动完成通知后，将目录窗口展示排到主队列，使用 `beginWithCompletionHandler`；窗口开始展示后再请求激活，不在创建窗口前抢先激活。
- 完成后停止事件循环并投递唤醒事件，避免异步回调只调用 `stop` 后仍等下一次用户输入。回到主函数统一输出结果并退出。[stop](https://developer.apple.com/documentation/appkit/nsapplication/stop(_:))。
- 明确区分选择、取消和系统中止；只有正常取消返回 `null`，系统中止返回失败。
- 保留中文/多语言声明、私有一次性应用包、单窗口限制、超时、路径校验和清理；不增加缓存服务、预热进程、固定延迟、循环抢焦点或系统偏好修改。

改动代码为 `src/workbench/application/native_picker.m` 和 `folder_picker.rs` 的验证部分。HTTP 契约、数据路径、配置格式和前端未改变。

## 3. 验证

### 3.1 原生窗口与产品接口

运行新构建的 `target/debug/codex-view --no-open`，隔离 HOME/USERPROFILE/CODEX_HOME/TMPDIR，CLI 指向不存在的测试路径。从真实配对接口建立 owner 后，连续三次调用 `POST /workbench/v1/application/pick-directory`，由 CUA 操作实际系统窗口，没有替换接口结果：

1. Escape 取消，返回 `{ "path": null }`。
2. Command-Shift-G 输入合成的 `project with spaces`，确认所在位置后点击“打开”，返回准确的绝对路径。
3. 再次打开，鼠标点击“取消”，返回 `null`。

三次均保留中文侧栏和按钮，接口正常结束，每次临时应用包随子进程退出被清理，Application 的 `runs` 始终为空。测试服务已停止，没有启动真实 CLI、发送模型请求或修改原生历史。

对最终源码仅添加时间戳的诊断副本，记录到启动完成约 0.124 秒、面板创建约 0.594 秒、展示调用返回约 0.630 秒、窗口成为 key 约 0.634 秒；实际键盘取消正常，completion 后事件循环立即返回。计时不包含构建与用户操作等待。

上述计时是本机样本，系统缓存和初始目录状态没有严格控制，不能当作“9.5 秒固定降低到 0.6 秒”的性能保证。key 通知也不等于全部目录内容加载完成；实际选择、取消、键盘交互由独立产品接口测试覆盖。本次完成启动顺序改进，首次慢启动是否在所有系统状态下消失仍需后续试用观察。

### 3.2 自动化与审查

- 新增事件循环 probe 回归先失败，修正后通过：验证启动通知、主线程、运行中的应用循环，以及异步完成后无需另一次输入便可退出。与生产路径共用退出函数。
- 该 probe 不创建面板、不主动请求激活，但 AppKit 仍需连接 WindowServer，因此标记为需图形登录的显式测试，未把环境失败静默跳过。
- Application 普通测试：20 passed、2 ignored（既有 Chrome 集成、新增图形会话 probe）。新增 probe 使用 `--ignored` 单独执行：1 passed。中/英/法语言、输出限制、超时与单窗口回归继续通过。
- 沙箱内最初因本地监听和 WindowServer 访问限制失败；在允许本机图形会话与监听的执行环境中重跑通过。所有数据测试均使用隔离目录。
- Clippy `-D warnings`、Cargo fmt、plist lint、debug 构建及 Git diff 检查通过。没有修改前端，不重复声明前端全量验证。
- 独立 GPT-6 只读审查未发现阻塞问题，主代理完成实际窗口和接口验证。

```sh
cargo test --offline --lib workbench::application -- --test-threads=2
cargo test --offline --lib native_picker_starts_and_stops_the_application_event_loop -- --ignored --nocapture
cargo clippy --offline --all-targets -- -D warnings
cargo fmt --all -- --check
plutil -lint src/workbench/application/native_picker.plist
cargo build --offline --bin codex-view
```

本地诊断与测试日志位于 `/tmp/codex-picker-latency/`，不提交实际文件列表、系统日志或私人路径到仓库。真实窗口证据为本次原生控件操作与 HTTP 结果，未宣称像素截图或严格冷启动性能回归。

## 4. 交付

无 migration、新配置或平台支持变化，工作台数据继续使用 `~/.codex-web`。分支 `codex/native-cli-live-workbench`，累积 R6 修改未提交、未推送。最新本地产物已重建，试用前需退出旧服务再启动，以避免单实例复用旧进程。继续停留在 R6-E 供试用。
