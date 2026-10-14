# 本地局域网接入文案验证（2026-10-14）

[验证索引](README.md) · [设备接入契约](../codex-native-cli-workbench-device-access.md) · [V2 实施计划](../v2-implementation-plan.md)

## 范围与改动

在 `codex/public-relay`、`fa094c2` 加既有未提交工作树上，将本机 Run 的“手机接入”统一改名为“本地局域网接入”。修改 `App.tsx` 导航/兼容按钮、`AccessPanel.tsx` 标题/无障碍名称/说明、`SettingsPanel.tsx` 端口标签和 `main.tsx` 配对错误提示；同步单元测试及现有 R1/R5/R6 浏览器探针选择器。面板明确局域网/Tailscale 直连本机，不经过公网中转站。

保留 LAN/Tailscale、二维码、开启/关闭设备访问、Run 作用域和撤销权限。没有后端逻辑、接口、配置、数据库 migration 或兼容版本变化。使用指南、设计、Changelog 与实施计划同步；历史验收中的旧名称保留。

## 验证结果

- 先增加导航、面板及配对失败提示的回归：实施前 4 项失败，实施后通过；新增设置标签断言，最终全部 303 项前端测试通过。
- TypeScript 检查通过。标准 `node scripts/dev.mjs build` 先生成 Web 再构建三个 binary，通过；最终构建任务 `ce39dcc2-4be5-410f-9816-17bf35901f3b`。保留现有大 JS chunk 警告，不将它描述为构建失败。
- 初次 Chrome 专项被共享资源时效门槛拒绝：设置测试文件在前一次构建后修改，任务 `8dc4ac12-e9c5-40d7-93ae-fec879b3885e` 保存失败记录。按标准入口重新构建后执行，未绕过或删除时效检查。
- 系统 Chrome 154.0.8037.100 的 `browser_device_access_qr_http_stream_input_and_revocation` 专项通过，任务 `72caa52c-b387-41f1-b419-d5ab79fc1320`，0 页面错误。验证新名称、固定码/重复扫码、二维码解码、双地址同 Run/PTY、普通 HTTP、合成流、中文输入、断网/刷新、390/320px、撤销与本机继续操作；截图对配对码/二维码脱敏。使用隔离临时 HOME/USERPROFILE/CODEX_HOME、合成地址和 shell PTY，不访问真实模型或用户任务。
- 文档、差异与暂存区二进制检查单独执行。暂存区为空，不以 checked 0 宣称工作树或历史二进制全面审计。本轮未重新执行完整 Rust 常规套件；改动仅前端文案/测试，使用前端全量、标准构建和上述真实 Chrome 专项验证。

## 交付边界

新文案已构建并在隔离测试中运行。没有重启用户现有本机测试实例、停止 CLI、开启其设备访问或修改公网部署；运行中的旧实例仍使用原内嵌前端，重启新产物后生效。未提交、推送或创建 PR。Chrome 手机视口不是本轮真实手机网络验收。
