# R6-E 修正：原生目录窗口真实本地化

日期：2026-09-23。用户截图证实前次无窗口语言探针通过后，真实目录选择器仍显示英文。本次修复保持 R6-E 范围，不进入多项目阶段。

## 1. 验证缺口与实现

Apple 明确说明 macOS 10.15 起 `NSOpenPanel` 总是由独立进程绘制，因此助手内 `NSBundle` 与 AppKit 的 `preferredLocalizations` 不足以证明真实面板的语言。[Apple NSOpenPanel](https://developer.apple.com/documentation/AppKit/NSOpenPanel)。Mach-O 内嵌 Info.plist 是受支持的方式，不能把前次问题归因为“不支持的 API”；系统应用 bundle 与进程内资源查询是不同的身份层，未打包应用可能没有 `NSRunningApplication.bundleURL`。[Apple bundleURL](https://developer.apple.com/documentation/appkit/nsrunningapplication/bundleurl)。

现在保留已有助手和接口，在私有临时目录创建最小应用包：

```text
codex-folder-picker-<随机值>/       权限 0700
  Codex Folder Picker.app/
    Contents/
      Info.plist
      MacOS/codex-folder-picker    权限 0700
```

物理 Info.plist 与嵌入的声明一致，增加 APPL 类型、版本和 plist 格式版本。助手在主线程执行 `finishLaunching`，然后激活和创建面板。正常运行仍直接执行包内程序；不调用 `open`、注册工具、codesign、shell 或运行时编译器。没有创建额外语言资源文件、固定中文文案或修改系统偏好。选择/取消后临时包随 TempDir 清理。

这是经真实窗口验证有效的组合修复；不对未公开的面板服务内部语言传播机制做进一步保证。

## 2. 真实产品接口与原生窗口

运行最新 `target/debug/codex-view --no-open`，使用独立临时 HOME/USERPROFILE/CODEX_HOME/TMPDIR，原生目录为空、CLI 路径故意不存在。从私有入口完成 owner 配对后，对产品 `POST /workbench/v1/application/pick-directory` 发起真实请求；没有拦截或替换窗口结果。CUA 绑定本次产品生成的随机临时 `.app`，读取实际原生面板并执行操作。

实际控件文本（省略个人路径、标签和目录内容）：

| 控件 | 读取结果 |
| --- | --- |
| 面板标题 | 打开 |
| 确认按钮 OKButton | 打开 |
| 取消按钮 CancelButton | 取消 |
| 侧栏 | 最近使用、共享、个人收藏、应用程序、桌面、文稿、下载、位置、网络 |
| 路径输入 | 前往： |

两种操作均通过：

- 点击“取消”：接口返回 `{ "path": null }`，Application `runs=[]`，本次临时应用包消失。
- 使用系统“前往文件夹”进入合成的 `project with spaces`，点击“打开”：接口返回该目录的准确绝对路径，`runs=[]`，临时应用包消失。

选择试验中一次组合快捷键操作提前得到取消，随后以读取表单、设置路径、确认目录的逐步操作完成选中验证；不把这次取消当作选择通过。截图工具报 ScreenCaptureKit -3811，未取得像素截图；此处证据是实际原生控件树及产品 HTTP 返回，不是先前的无窗口语言探针或网页替身。未保存真实用户文件列表或私人截图到仓库。

## 3. 自动化与审查

- 新回归先因缺少 `.app/Contents/MacOS` 结构失败；修复后确认物理 plist、bundle 路径、目录/执行权限、用完删除以及中/英/法语言资源选择。
- Application：20 passed、1 ignored（其中目录专项 3 项），包含 owner/Origin/instance 校验、单窗口和输出/超时限制、显式启动与生命周期回归。
- Clippy `-D warnings`、Cargo fmt、plist lint、Git diff 与 debug binary 构建通过。
- 独立 GPT-6 审查确认固定路径/内容、私有权限、错误与取消清理、子进程管理未引入新安全问题；主代理完成集成及真实窗口操作。

本次未修改前端或重新宣称前端全量测试。未启动真实 CLI、未发送模型请求、未写真实 `.codex` 或 `.codex-web`。临时测试服务和窗口已结束，正常使用的服务未被停止。

```sh
cargo test --offline --lib workbench::application::folder_picker -- --nocapture
cargo test --offline --lib workbench::application -- --test-threads=2
cargo clippy --offline --all-targets -- -D warnings
cargo fmt --all -- --check
plutil -lint src/workbench/application/native_picker.plist
cargo build --offline --bin codex-view
```

本地日志：`/tmp/codex-picker-bundle-before.log`、`/tmp/codex-picker-bundle-after.log`、`/tmp/codex-picker-bundle-application.log`、`/tmp/codex-picker-bundle-clippy.log`、`/tmp/codex-picker-bundle-build.log`。真实接口验证使用临时脚本 `/tmp/codex-picker-bundle-http-check.py` 与本次 CUA 工具记录；脚本自身不模拟 UI 输入，目录操作均由 CUA 完成。

## 4. 交付

无配置项、数据 migration、目录根改名或平台支持范围变化。当前分支 `codex/native-cli-live-workbench`，基线 `a1f96e5`；R6 累积变更仍未提交、未推送。

本次最新 debug 产物已重建。试用前退出旧 codex-view 服务，再运行新产物，避免单实例复用继续提供旧实现。后续保持 E 试用，R6-F 仍是独立的下一切片。
