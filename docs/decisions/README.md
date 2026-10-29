# 架构决策索引

[文档总索引](../README.md)

ADR 记录决定及其理由；适用边界以当前架构、专题契约和[支持范围](../guides/support.md)为准。编号空缺的旧决策可通过 Git 历史追溯，不得据历史内容恢复已退役范围。

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
- [ADR 0055：项目会话的分层展示与本机可撤销归属](0055-project-session-organization.md)
- [ADR 0056：可信公网中转与账号归属路由](0056-public-workbench-relay.md)（Accepted；实现与公网手机验收另列）
- [ADR 0057：独立端口的单工作台 IP 测试](0057-single-workbench-ip-trial.md)（Superseded by 0066；保留历史测试接入背景）
- [ADR 0058：管理员账号与单端口可信工作台前端](0058-admin-accounts-single-origin-relay.md)（Accepted；替代邀请流程，新模式同源边界和 schema 2）
- [ADR 0059：独立功能页面与中转断线恢复](0059-workbench-routes-and-relay-recovery.md)（Accepted；覆盖同页设置限制，保留固定授权和输入不重放）
- [ADR 0060：命令认证由原生 CLI 持有](0060-native-provider-command-auth.md)（Accepted；网页/网关登录与模型凭证分离，不执行或缓存命令凭证，保留有效路由来源门禁）
- [ADR 0061：中转账号删除与默认管理员无损改名](0061-account-deletion-admin-rename.md)（Accepted；schema 3、默认 windsyu、普通账号事务删除与防权限复活）
- [ADR 0062：本机工作台与中转的独立构建和部署入口](0062-independent-component-builds.md)（Accepted；按组件构建/检查，生产依赖隔离，共享协议与可信前端）
- [ADR 0063：本机后台服务与 owner 目录浏览](0063-background-service-directory-browser.md)（Accepted；macOS 用户服务、首次注册引导、完整退出与有界远端目录元信息）
- [ADR 0064：任务识别与人类对话优先的历史呈现](0064-human-conversation-presentation.md)
- [ADR 0065：以 main 为基础整合历史模块与公网中转 API 2](0065-main-history-relay-integration.md)
- [ADR 0066：按 CLI Run 复用的加密中转通道](0066-run-scoped-encrypted-relay-channels.md)（Accepted；可信前端、API/wire 3、单 origin，无明文降级）

- [ADR 0067：原生直连启动与可选模型捕获](0067-native-direct-launch.md)（Accepted；历史直连降级决定，共用模型捕获接入由 ADR 0068 更新）
- [ADR 0068：统一模型捕获与原生配置职责](0068-shared-native-model-capture.md)（Accepted；配置合法性交 CLI，共用捕获管线，补齐原生 Zstd 与缺媒体类型 SSE 观察）
