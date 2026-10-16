# codex-relayd：独立公网中转

此目录是独立 Cargo package，产物仅为 `codex-relayd`。负责账号门户、电脑归属、可信工作台静态前端及 HTTP/SSE/终端流转发；不运行 Codex CLI、PTY、模型代理或本机历史服务。电脑端出站连接器位于主 package 的 `src/workbench/relay`，不属于服务器。

从**仓库根目录**执行（构建需要共享 `crates/`、`web/` 和根锁文件，不能只复制本目录编译）：

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs build --component relay --release
node scripts/dev.mjs check --component relay
./target/release/codex-relayd --help
```

在目标 Linux 构建机原生构建后，只向服务器分发中转二进制及所需许可，不部署本机两个 binary，也不安装官方 Codex CLI。本地 macOS 构建得到的 Mach-O 不能复制到 Linux 运行；交叉编译工具链与静态运行库许可必须独立核验。共享前端在编译时嵌入，运行不需要 Node.js 或 `web/dist`。

- [组件选择与 AI agent 部署入口](../docs/guides/component-deployment.md)：两端能力、构建命令、部署顺序与交付证据。
- [Linux 中转部署与运维](../docs/guides/public-relay-deployment.md)：TLS、服务用户、密码初始化、schema 3、备份与恢复。
- [中转架构说明](../docs/architecture/relay.md)：依赖、模块源码导航、身份、安全、schema 与预算。
- [中转交互说明](../docs/interactions/relay.md)：账号管理、电脑授权、手机多电脑页面及 HTTP/SSE/终端 API。
- [本机工作台部署](../docs/guides/local-workbench-deployment.md)：安装电脑后端与绑定中转。

公网接入需要电脑后端在线且工作台 API 版本匹配。中转管理员不能跨账号进入电脑；服务器运营者是可信方，不提供端到端加密。不得在公网开放后端 8787 或把工作台用户目录复制到服务器。
