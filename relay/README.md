# Codex Relay

Codex View 的可选公网中继，独立程序为 `codex-relayd`。负责单源账号门户、电脑授权、可信网页资源及浏览器与电脑间的加密通道配对和密文转发；不持有业务通道密钥、不运行官方 CLI，也不保存工作台历史正文。

构建和部署需从仓库根目录操作，不能只复制 `relay/` 编译。完整说明统一在 `docs/`：

- [组件选择与独立构建](../docs/guides/component-deployment.md)
- [Linux 部署与运维](../docs/guides/public-relay-deployment.md)
- [架构与安全边界](../docs/architecture/relay.md)
- [交互与 API 契约](../docs/interactions/relay.md)
- [文档总索引](../docs/README.md)
