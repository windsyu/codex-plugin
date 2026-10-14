# ADR 0062：本机工作台与中转的独立构建和部署入口

Status: Accepted

Date: 2026-10-14

## Context

用户要求将公网中转与独立本机 codex-plugin 后端分开，能够分别编译、部署，并提供其他 AI agent 可直接遵循的两端操作说明。[ADR 0056](0056-public-workbench-relay.md) 已将服务器放入独立 package，但标准构建默认编译两个产品，原本机安装包流程也附带编译不分发的中转，文档没有统一的组件选择入口。

## Decision

- 保留同一 Git 仓库和 Cargo workspace，根 package `codex-local-observer` 负责本机 `codex-view` / `codex-observerd`，`relay/` package `codex-relayd` 负责独立服务器。两者不能通过 normal/build 依赖引用对方的生产运行模块。
- `src/workbench/relay` 是本机出站连接器，仍随本机产品交付。`crates/relay-protocol`、`crates/workbench-assets` 是共享协议和可信静态资源，不能反向依赖任一产品的运行模块；不复制前端、不从电脑加载网页脚本。
- 标准 `build/check` 新增 `--component workbench|relay|all`。默认仍为 `all`，保持原标准命令兼容；完整构建/回归显式使用 `--workspace`。直接 Cargo 命令的 `default-members` 改为仅本机 package；单独中转可用 `-p codex-relayd`。
- 两个独立构建都先生成 Web，再仅编译所选产品。组件检查仍执行共同的文档、工具、Web 和格式检查，Rust 编译/测试选择产品及共享包。本机现有 relay dev-dependency 保留跨端授权/撤销/流测试夹具，不是生产依赖，不因部署分离删除集成验收。
- `package` 默认且仅接受 `workbench`，只编译、包装已受测 macOS arm64 本机安装包；不伪造中转的跨平台归档支持。服务器在目标 Linux 构建环境使用独立 `build --component relay --release`，分发二进制和完整许可；特殊交叉编译/静态链接仍须专门验证。
- 为用户与 AI agent 提供组件选择指南、本机部署指南、中转独立入口和既有中转运维指南，区分编译依赖、运行依赖、账号认证、CLI 模型认证、数据归属及升级验收。构建工具不自动部署、重启真实任务或改变 GitHub 状态。

## Alternatives

两个 Git 仓库会引入协议、可信前端、锁文件与安全修复的跨仓库同步成本；用户要求的是分别编译部署，并未要求分库。把本机代码机械搬入第二目录不改善现有生产依赖边界。删除跨端测试可减少测试依赖，却会降低账号撤销及流路由的覆盖。使用 Cargo feature 把服务器塞进本机 binary 会破坏现有独立 package 和平台隔离。

## Consequences

服务器不需要官方 CLI、PTY 或本机历史；本机可以不安装、不部署中转而独立使用。共享源码仍需完整 checkout 和根锁文件，`relay/` 不是可独立复制的源码发行包。编译目标必须与部署平台匹配，macOS 中转产物不代表 Linux 部署已验证；工作台 API 仍严格匹配，部署分离不放松兼容或同源安全边界。

增加命令选择、构建顺序和 Cargo 生产依赖边界的自动化测试。验收及未验证平台记录见[组件验证报告](../validation/independent-components-2026-10-14.md)，不升级公网切片的整体 Passed 状态。本决定不修改运行时 API、认证、数据库 schema、历史数据或既有进程。
