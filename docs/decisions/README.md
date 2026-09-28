# 架构决策索引

[文档总索引](../README.md)

ADR 记录决定及其理由；适用边界以当前架构和[实施计划](../v2-implementation-plan.md)为准。编号空缺的旧决策见[归档索引](../archive/v2-before-cc-viewer.md)，不得据历史内容恢复已退役范围。

- [ADR 0039: 按 cc-viewer 重构为普通 CLI、模型代理与异步观察](0039-cc-viewer-style-runtime.md)
- [ADR 0040: 修复宽字符 resize 并隔离屏幕重建故障](0040-vt-screen-failure-isolation.md)
- [ADR 0041：用明确关联的原生记录补齐命令最终结果](0041-native-command-result-evidence.md)
- [ADR 0042：以规范 FileChange 补齐文件修改结果](0042-native-file-change-evidence.md)
- [ADR 0043：统一实时阅读项目与增量契约](0043-unified-reading-items.md)
- [ADR 0044：工作台异步记录与可验证历史](0044-workbench-asynchronous-history.md)
- [ADR 0045：CLI 版本提示与启动准入分离](0045-cli-version-compatibility.md)
- [ADR 0046：用户用量概览与折叠诊断](0046-user-facing-run-usage.md)
- [ADR 0047：将模型请求页面收拢为调用详情](0047-contextual-call-inspection.md)
- [ADR 0048：显式历史清理与统一 JSON 工作台配置](0048-history-cleanup-and-json-settings.md)
- [ADR 0049：工作台独立用户目录](0049-workbench-user-directory.md)
- [ADR 0050：固定项目根的文件、搜索与 Git 阅读](0050-read-only-workspace.md)
- [ADR 0051：同一工作台的多地址访问与配对](0051-workbench-device-access.md)
- [ADR 0052：连续滚动的只读文件阅读](0052-continuous-file-reading.md)
- [ADR 0053：历史首页、按项目启动与旧历史只读整合](0053-history-first-workbench.md)
- [ADR 0054：保留显式 provider bearer 与原生 API Key 认证](0054-native-custom-bearer-auth.md)
