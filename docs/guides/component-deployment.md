# 本机工作台与公网中转：独立构建和部署

[本机部署](local-workbench-deployment.md) · [中转运维](public-relay-deployment.md) · [开发指南](../development/README.md) · [支持范围](support.md)

本仓库交付两个独立运行组件：用户电脑上的 codex-plugin 本机工作台，以及服务器上的 `codex-relayd` 中转。它们可以分别编译、升级和部署，不必同时安装或启动。手机只有浏览器，不需要安装这两个组件。支持边界见[支持范围](support.md)；本指南不增加新的平台支持承诺。

## 选择要部署的组件

| 组件 | 源码及产物 | 安装位置与功能 | 不负责的事情 |
| --- | --- | --- | --- |
| `workbench` 本机工作台 | 根 package `codex-local-observer`；`codex-view`、`codex-observerd` | 用户电脑；本机网页、原生 CLI/PTY、模型代理、历史与项目阅读、可选局域网/公网接入 | 不作为公网账号服务，不自动安装/升级官方 CLI |
| `relay` 公网中转 | `relay/` package `codex-relayd`；`codex-relayd` | 可信服务器；账号、电脑登记、HTTPS 统一门户与有界转发 | 不启动 CLI，不调用模型，不扫描或保存本机历史正文 |
| 共享库 | `crates/relay-protocol`、`crates/workbench-assets` | 编译期共享协议和内嵌 Web 资源 | 不需要作为单独服务部署 |

`src/workbench/relay/` 是电脑端出站连接器，仍属于本机工作台，不是服务器实现。服务器生产依赖不包含根 package 的本机运行代码；两端使用同一仓库和锁文件不代表存在运行依赖。本机无需中转即可使用本机及局域网入口；中转可以先独立上线，但没有在线电脑时不会提供本机业务。

`codex-observerd` 是旧 Observer 兼容入口，不是公网中转；普通工作台只运行 `codex-view`。旧 `/v1` 保持只读，旧 `/v2` 不恢复。不要在中转服务器额外安装 Observer 来“补齐依赖”。

## 构建环境与命令

从受信任 checkout 根目录执行。构建基线为 Rust 1.95.0（rustfmt、Clippy）、Node 22.23.2、npm 和平台 C/C++ 工具；本机 macOS 目录助手需要 Apple SDK/Clang。首次先检查环境并安装锁定的前端依赖：

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
```

`doctor` 检查构建工具，不证明部署平台或网络可用。`bootstrap` 使用 `npm ci`；Cargo 使用 `--locked`。两组件均嵌入工作台前端，标准入口先构建 Web 再构建 Rust，不能省略 Node 构建步骤或混用旧页面与新后端。生产机器运行已构建产物不需要 Node、Rust 或源码。

```sh
# 用户电脑：只构建本机两个 binary
node scripts/dev.mjs build --component workbench --release
# 中转构建机：只构建中转 binary
node scripts/dev.mjs build --component relay --release
# 开发整体验证：构建两组件
node scripts/dev.mjs build --component all --release

# 组件专项与整个 workspace 的常规检查
node scripts/dev.mjs check --component workbench
node scripts/dev.mjs check --component relay
node scripts/dev.mjs check --component all
```

`build`/`check` 未传 `--component` 时仍为 `all`，完整 Rust 检查显式覆盖 workspace；不因 Cargo 默认成员改变而漏检 relay。直接 `cargo build` 的默认成员只包含本机 package；直接构建 relay 要显式 `-p codex-relayd`，且仍须先构建 Web。优先使用标准入口，不用裸 Cargo 命令绕过前端时效检查。

`check --component workbench` 会因跨端集成测试的开发 fixture 编译 relay，这不是本机生产依赖，也不要求上线 relay。`check --component relay` 检查中转及共享协议/资源，不编译根 package 的本机生产代码。组件检查仍包含共用的仓库/前端质量检查；它不是跨平台或真实公网验收。系统 Chrome 与官方 CLI 的专项测试须另行显式执行，见[调试指南](../development/debugging.md)。

离线环境先在联网构建机准备相同锁文件对应的 npm/Cargo 依赖，再给 `bootstrap`、`build`、`check` 或 `package` 加 `--offline`。离线模式不补齐缺失依赖，也不提供自动交叉编译工具。若在 macOS 编译 relay，得到的是 macOS binary，不能复制到 Linux 运行；Linux 部署须在匹配目标的环境构建并检查架构、动态运行依赖和许可。不能把编译成功称为 Linux 部署通过。

## 产物与分发边界

| 构建选择 | 默认 debug 产物 | `--release` 产物 |
| --- | --- | --- |
| `workbench` | `target/debug/codex-view`、`target/debug/codex-observerd` | `target/release/codex-view`、`target/release/codex-observerd` |
| `relay` | `target/debug/codex-relayd` | `target/release/codex-relayd` |
| `all` | 上述三者 | 上述三者 |

标准命令的诊断日志及包装输出登记在 `target/artifacts/<任务 UUID>`；以命令实际报告的路径为准，不把旧缓存中的同名 binary 当作本次成功产物。显式 target 构建的路径另包含 target triple。

```sh
# 原本的 macOS / Apple Silicon 本机安装包，不含服务器
node scripts/dev.mjs package --component workbench
```

`package` 默认同为 `workbench`，只支持当前受测的 `aarch64-apple-darwin` 本机包；包含 `codex-view`、`codex-observerd` 与 `code-view` 启动脚本，不含 `codex-relayd`。`package --component relay|all` 不受支持。中转目前采用独立 `build --component relay --release` 后按[中转运维指南](public-relay-deployment.md)分发 binary、许可、版本/校验记录和部署配置；不承诺新的 Linux 安装归档或自动交叉编译。静态 Linux 分发还须保留实际工具链/系统运行库通知，不能只收集 Cargo/npm 许可。

本机包的校验、安装、PATH 和卸载见[安装指南](installation.md)。正式发布仍要求干净、已评审、CI 通过的 main tag；dirty checkout 的本地构建不是正式 release。构建产物不得提交 Git，不要为清理空间整体删除 `target` 或数据目录。

## 三条接入路径与两类认证

1. 本机：启动 `codex-view` 后通过私有 entry 文件兑换本机 owner 入口，默认仅监听 loopback。不需要公网账号。
2. 局域网/Tailscale：在具体 Run 的“本地局域网接入”页显式开启设备访问，使用该 Run 的二维码/配对码直接连接电脑。授权只限该 Run；不经过中转，不提供全局历史/共享设置。LAN HTTP 不加密，不自动修改防火墙。
3. 公网：电脑在本机 `/relay` 登录可信中转账号并开启出站连接；手机或其他浏览器访问同一 HTTPS 门户，登录后选择属于自己的在线电脑。新部署使用 `--single-origin`，门户及 `/g/{gatewayId}/` 业务共用一个 origin，无需每台电脑开公网端口。

公网账号认证保护网页/电脑连接，原生 CLI 的模型认证只在用户电脑按 provider 配置执行。二者互不替代：登录中转不能修复模型登录，模型 API Key 不可作为中转密码，也不可上传到服务器。官方 CLI 负责模型凭据与 `auth.command` 的执行/刷新，工作台只透明代理本次模型请求。当前支持的 provider 和验证限制见[本机部署](local-workbench-deployment.md)。

中转后端只转发浏览器与电脑的业务密文，按 CLI Run 复用；中央前端与授权方仍必须可信，恶意运营者修改脚本或授权不在防护范围内。仅连接用户明确接受的服务器，三端配套升级至 API/wire 3，无明文降级；不得在代理日志记录凭据或页面 grant，见 [ADR 0066](../decisions/0066-run-scoped-encrypted-relay-channels.md)。

## 中转独立运行的最小交接

中转目标机只需要匹配架构的 `codex-relayd`、其实际动态运行库、私有数据目录和受信任 TLS 反向代理；不需要官方 CLI、模型 Key、工作台历史或 Node。`relay/static/` 是服务器门户资源，工作台 Web 则经共享资源 crate 编入 binary，不需要把源码 Web 目录部署为独立网站。

新部署的启动参数形状如下；这只是配置示例，不是要求 agent 自动执行：

```sh
codex-relayd --help
codex-relayd --version
codex-relayd --data-dir /var/lib/codex-relay \
  --portal-origin https://app.example.com --single-origin \
  --trusted-client-ip-header x-relay-client-ip \
  serve --bind 127.0.0.1:8787 \
  --bootstrap-password-file /var/lib/codex-relay/bootstrap-password
```

服务器没有隐式默认数据目录，必须显式传 `--data-dir`；其中 `relay.sqlite3` 保存账号/登录/电脑登记及可能的 WAL/SHM，不是本机历史库。启动配置由部署者的 systemd 参数/非秘密环境文件和 TLS 代理配置管理，不读取本机 `config.json`。仅支持单 origin，`--single-origin` 可显式声明；旧域名/固定 gateway 双入口参数与模板已移除，不可混用旧配置。

私有密码文件由服务所有者按中转指南准备，首次初始化 `windsyu` 管理员后移除文件与对应启动参数；不得把密码直接放命令行/环境。实际服务用户、目录权限、TLS Host/可信来源头、已有配置合并、账号创建与撤销、管理员离线恢复、schema 3 备份与回滚规则均只按[中转运维指南](public-relay-deployment.md)执行；IP/非默认 HTTPS 端口按其[单 IP 章节](public-relay-deployment.md#单-ip-与受限服务器部署)。不要公开 `8787` 后端、启用生产 `--allow-insecure-loopback` 或关闭 TLS 验证来消除登录错误。

## 配置、升级和验证

本机配置/历史/出站登录与 relay 账号库是独立数据，分别备份，不能相互复制或迁移：本机路径见[本机部署](local-workbench-deployment.md)，服务器 `--data-dir`、SQLite 一致性备份、schema 迁移与恢复见[中转运维](public-relay-deployment.md)。两端版本可以分别升级，但共享协议/资源须兼容；门户的“版本不兼容”不是离线，不能绕过后强行打开。升级中转不应停止本机 CLI；升级本机应用则会停止其所属 CLI，必须征得运行所有者同意。

部署后应分别验证本机历史/明确启动、模型中间态与保存状态，服务器 TLS/账号/权限，电脑出站连接，最后验证手机门户/跨电脑及撤销。[支持范围](support.md)记录受测环境与限制，不能用于宣称后来构建、真实手机、多电脑或新环境已经通过。本指南给出操作流程，不是本次部署成功记录。

## 交给其他 AI agent 的执行边界

先阅读 [AGENTS.md](../../AGENTS.md)，检查工作树、分支/remote 和用户授权，明确本次仅编译哪一组件、实际目标架构、安装路径、服务用户、数据目录、已有进程与可接受停机。没有明确部署授权时仅构建/检查并报告，不安装、重启、改防火墙、上传文件、创建账号或操作真实数据库。

不要读取或输出私人 entry、模型凭据、`relay/state.json`、密码文件或 Cookie。不要自动执行管理员恢复、账号删除、数据库撤销 SQL、历史清理、schema 回退或整体目录清理；不要使用仓库示例覆盖现有 Caddy/systemd 配置。测试使用独立临时 HOME/USERPROFILE 和 CODEX_HOME，不能以真实用户数据做 fixture。

交付报告须列出源码 revision/dirty 状态、组件/target/产物、实际运行的检查和结果、未执行的网络/真机验证、配置或迁移影响、停机/备份和回滚限制。只有命令真实成功与目标环境验收证据齐全时，才报告对应层级完成；不把 `--help`、单元测试或同名旧产物当作部署证据。
