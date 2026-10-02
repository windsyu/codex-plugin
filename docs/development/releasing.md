# Release 包装与发布

[开发指南](README.md) · [安装指南](../guides/installation.md) · [执行规范](../../AGENTS.md)

## 本地打包

当前版本 `0.3.0`，本机受测目标为 `aarch64-apple-darwin`。固定工具链、锁文件和前端优先顺序继续沿用标准入口：

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs check
node scripts/dev.mjs package --offline
```

`package` 重新生成 Web 资源并编译两个 release binary，检查 binary 的版本和动态库依赖，然后在 `target/artifacts/<UUID>` 生成压缩包、`SHA256SUMS` 与解包目录。该任务按独立构建产物登记，保留 30 天；需要长期留存时由所有者明确复制或固定保留。只允许当前已受测的 macOS / Apple Silicon，不自动交叉编译或推导其他平台支持。

打包构建显式指定 `--target aarch64-apple-darwin`，并读取同一 target 的 release 目录；不受 `CARGO_BUILD_TARGET` 或 Cargo 默认 target 配置影响，不从另一个构建目录拾取旧的同版本产物。

生成后自动在独立 HOME/USERPROFILE/CODEX_HOME、中文及空格目录中解压安装，以合成 CLI 验证当前目录/指定路径启动、同项目复用、零 CLI 历史入口、内嵌网页资源和 SIGINT 回收；不启动真实私人 CLI 或调用模型。验证失败的任务保留诊断，不报告打包成功。

包内包括项目入口 `bin/code-view`、历史入口 `bin/codex-view`、兼容入口 `bin/codex-observerd`、安装器、版本/来源 manifest、使用说明、锁文件、项目许可、vt100 补丁说明、Codicons 归属及第三方许可。无需额外携带 `web/dist` 或目录助手。`code-view` 是保持 cwd 的 shell 包装器，复用 `codex-view --project`，不增加 Rust binary 或运行协议。

许可收集使用当前 host 的 Cargo metadata，包含解析依赖及 build/test 依赖；npm 部分只收集已安装的运行依赖，未分发的前端开发工具不进入运行包。逐依赖保留实际 LICENSE/COPYING/NOTICE/COPYRIGHT 文件，包含嵌套原生材料；缺失许可、锁文件不匹配、符号链接或越界会使打包失败。`licenses/INDEX.json` 记录版本、许可表达式与通知位置，不含本机源码路径。维护者仍需复核实际许可内容。

`manifest.json` 记录版本、target、sourceCommit 和 sourceDirty；本地工作树有改动时允许生成可试用包，明确标为本地来源，不伪装为已发布 main tag。包内与归档校验和分开，安装器校验包内内容。当前包没有 Apple 签名、notarization 或自动更新。

## 发布门槛

正式 GitHub Release 必须从干净、已评审且 CI 通过的 `main` tag 生成，tag 与 Cargo/Web 版本一致。核对常规检查、实际解压安装/项目启动、第三方许可、支持范围及已知问题后准备 release notes；GitHub Release 上传压缩包与 SHA256SUMS，二进制不进入 Git。

本地 `package` 不执行 push、创建 Issue/PR、merge、tag 或发布，不安装到真实用户目录。远程操作按当前用户授权执行，不因添加维护命令而扩大授权。发布前需区分此前 R6 功能验收、新包的安装验证和远端 CI 的实际结果。

本次包装不升级官方 CLI、不修改原生模型/权限、不迁移数据库，也不扩展 V3、平台或 provider。当前功能与兼容限制见[支持范围](../codex-native-cli-workbench-support.md)。
