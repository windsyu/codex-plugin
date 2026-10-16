# 用户电脑上的本机工作台部署

[组件选择与独立构建](component-deployment.md) · [安装指南](installation.md) · [使用指南](usage.md) · [支持范围](../codex-native-cli-workbench-support.md)

本指南部署用户电脑上的 codex-plugin 工作台。它打开本机网页，在用户明确选择项目后运行已安装的普通官方 Codex CLI，通过 PTY 保留原生输入/审批，代理本次模型连接并异步保存脱敏阅读副本。历史、文件、搜索和 Git 阅读在电脑上完成；公网中转只是可选接入方式。本机不需要安装或运行 `codex-relayd`。

非目标：自动安装/升级或修改官方 CLI、代替模型服务登录、迁移/删除原生历史、向公网裸露本机 owner 端口、无人授权自动启动项目、Windows/Linux 本机支持承诺。当前受测本机为 macOS / Apple Silicon；其他环境和浏览器限制以支持范围为准，不能由中转的 Linux 部署推导 Linux 本机工作台可用。

## 1. 选择安装包或源码构建

已有经过校验的 macOS 本机包时按[安装指南](installation.md)安装；包内 Web 与目录助手已嵌入，运行无需 Node/Rust/源码。发布的旧包保留其当时功能，不因当前文档更新自动获得新公网/认证功能；核验实际版本及其验证记录。

从源码部署时，在仓库根目录运行：

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs build --component workbench --release
node scripts/dev.mjs check --component workbench
./target/release/codex-view --help
./target/release/codex-view --version
```

构建需要固定 Rust 1.95.0、Node 22.23.2、平台编译工具；macOS 还需要 Apple SDK/Clang。标准入口先构建 Web；更改前端后须重建 Rust，不能用旧后端搭配新页面。`check` 会为跨端 fixture 编译 relay，但生产 binary 不因此依赖 relay 服务。源码构建和产物管理细节见[组件指南](component-deployment.md)。

产物为 `target/release/codex-view` 和旧兼容入口 `target/release/codex-observerd`，日常只启动前者。需要原 macOS 安装包时执行 `node scripts/dev.mjs package --component workbench`，从命令报告的受管产物目录取包并校验；包不含中转服务器。只运行源码 binary 不会安装 `code-view` 脚本或管理 PATH。

## 2. 准备运行依赖与模型配置

打开历史首页无需 CLI 或模型登录；启动项目才需要已安装的官方 Codex CLI。使用 `--codex-bin` 可指定路径，不下载、不升级。代码搜索需 PATH 中的 `rg`，Git 阅读需 `git`，缺失只使对应阅读能力不可用。

CLI 原生配置位于其 `CODEX_HOME`（未设置时使用原生默认），与工作台 `.codex-web` 分开。`--profile named` 选择 `$CODEX_HOME/named.config.toml`；工作台 JSON 只保存启动偏好，不能替代 provider 配置。不得把个人配置或认证文件提交仓库或复制到中转。

当前模型适配器为 `unmanaged-custom`：原生 `custom` provider、Responses、显式上游、File credential store，并从两条互斥认证路径选一条：

- 非空静态 bearer，`requires_openai_auth=true` 或 `false`；保留原生 File API Key。
- 原生 `auth.command`，`requires_openai_auth=false`，可选字符串数组 `args`；命令/token 由官方 CLI 执行、缓存与刷新，工作台不执行命令或读取输出。与静态 bearer、env key/header、AWS 和 `true` 互斥。

工作台检查缺失或 API-key 形状的 `auth.json` 以排除 cloud-managed 路由变化，不从中取得命令 token。managed/ChatGPT、其他 provider、环境认证、AWS 和真实 provider WS 未纳入支持。无法确认路由时项目启动拒绝并说明原因，历史首页保留；不要修改模型/权限、关证书校验或改全局配置绕过。新增命令认证有安装版 CLI 0.162.1/合成 provider 证据，真实 provider 和公网发送仍待验，见[支持范围](../codex-native-cli-workbench-support.md)与 [ADR 0060](../decisions/0060-native-provider-command-auth.md)。

模型 Authorization 只在电脑上的原生模型路径透明转发，不进入观察、页面或 journal；中转账号登录不为模型登录。工作台不读取动态 token，不能保证按精确值脱敏正文主动回显的动态凭据，勿让模型/工具输出凭据。

## 3. 启动、复用和生命周期

以下路径为占位符，应替换为已核验的本次产物和现有项目绝对路径：

```sh
# 只打开/复用项目概览与历史，不启动 CLI
/path/to/repository/target/release/codex-view
# 明确在已有项目启动新对话
/path/to/repository/target/release/codex-view --project /path/to/project
# 指定已安装 CLI 和命名配置；不自动打开浏览器
/path/to/repository/target/release/codex-view --project /path/to/project \
  --codex-bin /path/to/codex --profile named --no-open
# 重新打开已运行应用的私有入口，不创建新 CLI
/path/to/repository/target/release/codex-view open /path/to/private/instance.json
```

默认概览为 `/`，历史 `/history`，明确启动 `/launch`，共享设置 `/settings`，公网连接设置仅本机 `/relay`。具体 Run 使用 `/runs/{runId}/{chat|terminal|files|search|git|history|usage|settings|access}`。CLI flags 还包括 `--provider-profile unmanaged-custom`、`--resume <UUID>`、`--entry-file <新私有文件>`、`--config-dir <目录>`、`--data-dir <目录>`；只有明确项目/恢复请求创建 CLI，启动偏好 flag 本身不会。恢复核验原生会话来源和目录，不自动续最近任务、不重放历史或输入。

同配置目录与数据根重复启动会核验私有实例入口并复用，不靠删除锁来强制第二个实例。输出的 `entryFile` 是私有认证材料位置，不是可公开分享的文件；`open` 用它打开页面。不要读取/打印其内容或传播带配对凭据的 URL。自定义 `--entry-file` 须为新的私有文件，正常退出移除。

已安装包的 `code-view` 默认把当前目录作为明确项目启动请求；`code-view /path/to/project` 指定目录，`code-view --history` 只读首页。它与无参数 `codex-view` 语义不同，不要在自动化中混淆。

需要远端长期连接时，使用 `/path/to/codex-view start`（安装包对应 `code-view start`）建立 macOS 用户级后台服务；用 `status [--json]` 检查、`open [--relay]` 打开、`stop` 完整退出。`autostart on/off` 只控制下次登录。第一次交互 `start` 无电脑绑定时打开本机注册页；已有绑定遵守 enabled 开关，非交互/系统启动不弹窗。后台不自动启动/恢复项目，完整操作见[使用指南](usage.md#后台开启远程注册和退出)。LaunchAgent 使用固定可执行文件和有效配置路径，升级前停止所有所属作用域，不直接覆盖运行中的 binary。

普通启动器保持前台运行，后台模式则交给 LaunchAgent；关闭页面、回首页或关闭公网连接不停止 CLI。后台 `stop` 或前台 Ctrl-C/正常退出会结束应用及其所属 CLI；网页“停止当前运行”只停止选定 Run。SIGKILL、断电或工具脱离进程组不在同步回收保证内。现有项目有活动 Run 时进入原 Run，不另启；需要换运行或升级必须先得到该运行所有者的停机同意。

## 4. 配置和数据分离

| 数据 | 默认位置 | 自定义与保护 |
| --- | --- | --- |
| 工作台配置 | `$HOME/.codex-web/config/config.json`，同目录 schema/保存前备份 | `--config-dir` 指定目录；命令行覆盖不反写；不覆盖已有配置 |
| 工作台记录/派生索引 | `$HOME/.codex-web/history/` | `--data-dir` 或 JSON `storage.dataDir`；父目录须已存在，私有用户所有；不是完整终端录像 |
| 出站中转登录 | `<有效配置目录>/relay/state.json` | 默认 `.codex-web/config/relay/state.json`；私有目录 `0700`、文件 `0600`；含电脑授权，不能作为普通配置分享 |
| 应用发现/锁 | `$HOME/.codex-web/runtime/<scope>/` | `instance.lock`、私有 `instance.json`；不手工删锁强行复用 |
| 后台服务登记/诊断 | 同一 `runtime/<scope>/` 下 `service.json`、`service.plist`、`service.log`、`service.lock` | 登记有效路径与非秘密启动环境，绑定本次可执行文件；不手工修改或删锁接管 |
| 可选登录自启 | `$HOME/Library/LaunchAgents/io.github.windsyu.code-view.<scope>.plist` | 仅 `autostart on` 写入，由 `off` 移除；保持当前服务运行状态 |
| 原生 CLI 配置/认证/会话 | 原生 `CODEX_HOME` | 官方 CLI 管理；工作台不自动搬迁或清理 |

历史根中 `runs/` 保存分段记录/快照/上下文，`index.sqlite` 和 `library/catalog.sqlite` 是派生索引；`cleanup/` 可能含未完成清理任务。历史保存失败不阻断 CLI，实时显示不代表 fsync 成功，重启不保证未保存尾部完整。清理默认关闭，不以容量维护为由删除私人历史或原生会话。

旧 `$CODEX_HOME/workbench/config` 和 `$CODEX_HOME/workbench-data-v1` 留在原位，不自动迁移；确需读取时分别显式传原配置/历史目录并核验。旧 Observer 数据由独立只读 reader 登记，勿运行 `import` 或修改 schema 来“升级本机部署”。

更改程序/profile/浏览器偏好/数据根需要明确退出并重启应用后生效，不移动历史。修改数据路径还会改变实例作用域，不是创建第二个应用控制同一会话的理由。目录约定虽包含 Windows，当前 Unix 终端/锁实现没有 Windows 运行验收。

## 5. 可选手机入口

只需同网访问时，在 Run 的 `/runs/{runId}/access`“本地局域网接入”显式开启设备访问，手机扫描该 Run 的码。LAN IP 和可用 Tailscale MagicDNS 共用端口与 Run，不需要中转或 Tailscale Serve；Tailscale 由用户环境准备。授权仅限此 Run，无全局历史/共享设置权限；Run 结束失效。`access.port` 为 0 自动选择，固定端口允许 1024–65535，下次开启生效。不自动开防火墙，LAN HTTP 不加密。

需要跨公网统一电脑列表时，在本机 `/relay` 填可信服务的 HTTPS 地址和电脑名，明确“登录并开启公网接入”。浏览器登录中转账号，核对一次性码、电脑名和完整权限后确认。手机访问同一 HTTPS 门户登录并选在线、版本兼容的电脑；单 origin 模式打开 `/g/{gatewayId}/`。电脑仅向服务器建立出站连接，无需将 owner 端口做公网转发。账号首次登录须改临时密码；不将模型凭据填入中转页面。

关闭公网接入保留登录，退出登录并撤销接入清理本机许可并尽力撤销服务器登记；失败时按提示到门户撤销。改密/禁用使旧页和电脑授权失效，需重新登录。短断线可重建连接与读取状态，不重放按键或启动/停止请求，不自动恢复 CLI。服务器完整 TLS/账号/备份配置只在[中转运维指南](public-relay-deployment.md)维护。

## 6. 升级、备份与验收

升级前记录旧 binary 版本/校验、启动参数和有效数据路径，确认运行所有者同意停机，后台用相同作用域的 `stop`，前台在原启动器正常退出；确认自有 CLI 已退出后，一致性备份配置及工作台历史根。备份包含私有 relay 登录，须加密/私有保存；不是可公开的调试包。原生数据需要另行按用户策略备份，不把它自动上传服务器。不要只复制仍在写入的 SQLite 主文件而漏 WAL。

安装经过校验的新产物再启动；本机与中转可以分别升级，但须核验协议兼容。配置/数据格式升级不能假定旧 binary 可回读；回退前核验兼容与备份，勿覆盖失败现场。工作台卸载保留 `.codex-web` 和原生数据；清理它们是另一个需要明确授权的任务。

部署验收至少检查：正确版本/产物；零 CLI 首页；明确启动正确 cwd/CLI；原生目录信任/审批；模型响应结束前的中间内容；保存与缺口状态；关页面 CLI 保活；正常退出回收。开启可选入口时另验 LAN 或门户登录/电脑归属/终端输入/撤销/断线。测试应使用合成模型及临时 HOME/USERPROFILE、CODEX_HOME；真实模型请求、真实项目操作和真机输入要用户明确授权，不能为“冒烟”自动发送。

`--help`、构建、常规测试只能证明各自检查范围，不证明真实模型、手机、多电脑或新平台成功。此指南未声明本次已执行部署验收；实际结果应包含环境、版本、命令、失败和未验证项，记录到对应 PR、CI 或受管产物；持续影响支持范围的限制应更新[支持文档](../codex-native-cli-workbench-support.md)。其他 agent 执行前还须遵守[组件指南的授权边界](component-deployment.md#交给其他-ai-agent-的执行边界)，不得停止已有应用、删除锁/历史、读取私人认证或覆盖用户配置。
