# 独立页面、连接恢复与专用中转升级验证（2026-10-14）

对应 [RQ18](../v2-implementation-plan.md) 与 [ADR 0059](../decisions/0059-workbench-routes-and-relay-recovery.md)。本记录覆盖本地回归、配套产物、指定 ECS 升级与自动重连，**不代表公网真实模型消息或真实手机验收完成**。阶段状态只在 V2 实施计划维护。

## 环境与本地检查

分支 `codex/public-relay`，基础 commit `fa094c23b3742a6bb6cb475c7ac924fd7357582f`，账号/单端口及此次增量未提交；没有推送、PR、发布或 V3 修改。Rust 1.95.0、Node 22.23.2，安装版 Codex CLI 0.162.1、系统 Chrome 154.0.8037.100。自动化使用独立 HOME/USERPROFILE/CODEX_HOME 和合成模型，不写真实原生历史。

标准 `node scripts/dev.mjs check` artifact `a97404cb-99c4-4f60-8c5f-6b4973842e6b` 已完成 success、lease 清除：维护工具 82/82，前端 36 文件 296/296，TypeScript、fmt、Clippy、debug/release 二进制构建通过。Rust all-targets 合计 530 passed、0 failed、56 ignored；ignored 不算通过，其中工作台 library 383/51、Observer 94/1、relay library 41/0（通过/忽略）。前端 chunk 超过 500 kB 的非阻断警告保留。

标准检查后只改最后原生探针的导航定位、包装许可 helper/测试和文档。最后原生 exact 测试实际复跑通过；包装回归先红再绿 10/10，完整 `node scripts/dev.mjs test-tools` 85/85、0 failed，artifact `74bec2cc-2588-424f-bccc-535f0e43088b`。后续工具检查不冒充重新执行全部 Rust/前端测试。

| 专项 artifact | 实测范围及边界 |
| --- | --- |
| `dea3787f-ddaf-49ea-a434-d0cd652c7948` | 安装版 CLI＋Chrome＋合成模型：键盘/焦点、picker 取消、approval/question、刷新、短断线/丢 ACK 不重放、同进程/epoch、取消后确认停止与停止后阅读。1 passed，pageErrors/faults 均 0，4 次模型 fixture 请求匹配。 |
| `aee3e156-2418-4b2e-9e38-31640f64a6b8` | 新建/恢复同一原生会话、零自动输入、手工目录输入/取消，1024/736/390/320 宽度；picker API 为 stub，不代表原生文件夹窗口验收。 |
| `31dcb6dc-a0d7-41e0-9629-1effd9addf89` | 未归属会话恢复并保留记录 cwd，入口/键盘路径通过。 |
| `a2690865-93ac-45b2-a3e2-824f9f21dbff` | 合成三类历史、分类/分页、失效项目恢复、pending 后自动阅读；DOM 最多 48 条，0 WS/外部请求/pageErrors。 |
| `136f928c-c2dc-4beb-a347-94b77cef8f25` | 安装版 CLI 双项目流/输入权/文件/用量/工具/停止隔离、保存、设备 QR 与 canonical 刷新；1440/390/320 退出页，pageErrors 0。 |
| `a0b94a08-47f3-4160-9f2e-9f65c45212ff` | 单 origin 三工作台 Chrome：深链接/刷新/返回、跨电脑/账号拒绝、同 Run 终端保活、0 隐式启动/输入；CLI 是 shell fixture，不是真实三台电脑或真实模型。 |

standalone 文档 200、scoped pair 404 的问题先复现后修复。HTTP/WS 两项与共享 Run 隔离复验通过；`workbench::web::` 31 passed、0 failed、10 ignored。精确白名单及原始 Host/Origin/Ingress/权限边界保持；独立审查冻结的 `web.rs` SHA-256 为 `19438e08d3203b0c9c2ef7395a46f17c6124ed6a552d95702a743895105c90f9`，部署后本地复核一致。

LaunchPanel 初始焦点和调用记录关闭后的焦点恢复也有先红后绿回归。恢复的有界退避、租约/取消 fencing、连续时钟失败关闭有合成测试与独立审查；不能据此宣称真实睡眠或公网网络黑洞已测。

## 最终 Linux 包与许可整改

固定 Zig 0.14.1，目标 `x86_64-unknown-linux-musl`，Cargo `--locked --offline`；前端先生成，再构建 Rust。初版候选 `735d91b4-c369-450e-afae-23e2155980ca` 缺少 Cargo/npm 索引之外的静态运行库通知，未部署。补齐 musl、LLVM libunwind、Zig compiler runtime、Rust 标准库/内建函数通知后重包，没有更换冻结二进制。

最终 artifact `d755a016-cfc4-4090-851d-b5be5b228f74` 独立审查通过：3864 成员/3337 普通文件，单根，无穿越、重复、链接或特殊文件；目录/二进制 0755，其余 0644。292 项索引包括原 288 项和四类运行库，新增 2843 份文本与原来源逐字匹配；保守保留单文件通知，不从 stripped ELF 推断最小许可集合。当前 8 份前端资源完整嵌入；静态 x86-64 ELF 无 PT_INTERP/PT_DYNAMIC。macOS 正式分发 sysroot 的核查不在本包证据内。

```text
archive   86115d2fdede332518d2da84d76bc859b76b67a1586e8bc0402e1afa8d2236ff
binary    70a669e55f193b370212c36f2c90eb80e9d139876d1b85c19c15678d46f007b4
manifest  a6439376da9dad8d554d6af316ea50092278631404ace65984fbdc6bcb769d4e
checksums dd5731a6ba3250ef82daf250a29781bbae477bb1740203200fe5849e79cda0a0
```

manifest 标记 sourceDirty；这是测试部署包，不是从已评审 main tag 发布的正式 release，也没有签名或公证。

## 实际升级与恢复副本

ECS `i-bp16nvvpdk423m48yewl`，公网 `https://47.97.44.196:9443/`，专用 `codex-relayd-ip-trial.service`，loopback 8787。部署日志 artifact `97cf8715-a88d-401f-9f35-d939a7f6a43f`。上传后验证固定包/控制脚本哈希；root 私有新目录以 umask 022 解包，拒绝既有目标，不导入 xattr/ACL，并复验全包。

首次 preflight 拒绝旧分发归档遗留 UID 501/GID 20，未停止服务或放宽检查。旧二进制、manifest/checksums 与可信初次构建 artifact `3748e522-6a48-45a5-8173-dd07707227a0` 匹配；803 项原模式、同设备、无链接/硬链接/ACL/capability/挂载已检查，UID 501 无登录身份或进程。只修正精确旧分发树为 root:root，字节/模式及旧 PID 1314177 保留；严格包检查与正式 preflight 通过。

本次 **schema 2 → schema 2、结构不变**。独立审查脚本停止并确认进程/listener 退出后保存完整私有目录和一致性 SQLite 快照，加密/HMAC 备份并 fsync；只切换专用服务二进制路径。失败策略只回退二进制/启动配置，保留当前数据库中的并发登录、改密和撤销，不自动恢复旧快照。

- 新分发：`/opt/codex-relay-trial/releases/d755a016-cfc4-4090-851d-b5be5b228f74/relay-upgrade`。
- 新备份：`/opt/codex-relay-trial/backups/d755a016-cfc4-4090-851d-b5be5b228f74-schema2`；独立密钥留在服务器 root 私有目录，密码/密钥/凭据未输出或传出。
- HMAC 验证后实际解密到全新私有 `restore-check`；恢复副本 schema/完整性/FK、账号/电脑保护字段、原配置/分发字节通过，未覆盖在线数据库。
- apply 返回 0。启动首次 readiness 短暂 connection refused，随后有界 readiness 与本地/公网检查通过，未触发回退。
- 新 PID 1347486，active/running、NRestarts 0；MemoryLimit 402653184（384 MiB）、CPUQuotaPerSecUSec 350 ms、TasksMax 64、KillMode control-group 保持。系统的 MemoryMax infinity 不意味着 MemoryLimit 被移除。
- unit/environment 与备份逐字一致，精确进程参数通过。没有 admin 初始化/改密、TLS reload、容器/443、安全组、防火墙或其他服务变更。
- 现场 schema 2、3 账号/1 admin、1 电脑/API 1；上线前后密码、角色/禁用/强制改密、授权代际、电脑归属/凭据等保护字段不变。这不是实际三电脑现场验收。
- 公网 TLS 校验开启：六个门户文档为 200，匿名 account/gateways/admin-users API 均 401。文档 200 不代表已登录或拥有管理权限。

## 本机重启、目录与未完成验收

确认 activeRuns=0 后正常停止旧测试 launcher，以最新 debug 产物重启同一临时 config/history；保留用户授权的真实 HOME/USERPROFILE/CODEX_HOME，不修改原生配置或历史。新 PID 68135、入口 `http://127.0.0.1:58565/`，日志 artifact `e16a09f1-628c-4cb9-9875-0a9056946352`；debug SHA-256 `a6500ad7c0dc36eb6d3a058a8ecbd26d95b69c7a9e9434c94974d8d23a4ada01`。中转升级后无需重新认证自动恢复 online，enabled/accountBound 为 true，登记保留。

用户授权临时实例的真实目录预检：ASCII `~/workspace/nacos/` 和 `/Users/windsyu/workspace/nacos` 均 200，canonical 相同；全角 `～/`（U+FF5E）为 422，ASCII `~` 是 U+007E。这不证明实际 CLI 启动或真实模型回复成功。

独立 headed Chrome 使用 390×844、touch、incognito context，不带用户既有 Cookie。初次检查未登录；后续同一存活浏览器会话实际返回 `signedIn=true`、`ownTestComputer=1`、`tabs=1`，证明用户登录后能看到本机临时测试电脑，但不证明已进入工作台或发送消息。未读取密码库、复制其他 Cookie、使用 bootstrap 密码或重置账号。

该独立浏览器未出现在桌面浏览器操作接口的可控制标签列表中，现有 runner 仅支持状态、刷新、截图和关闭，不能直接执行后续终端操作。另开的系统 Chrome 原生无痕窗口已确认存在，但界面操作被窗口切换中断，尚未完成门户导航；原已登录会话保持运行，没有为接管而清除 Cookie 或关闭窗口。公网安装版 CLI 启动/真实消息/SSE、路由/刷新/新标签、短断线和有运行中 CLI 的中转重启仍待在可操作的无痕窗口继续验证。

真实手机/移动网络、证书续签、实际多电脑公网并行、真实睡眠/公网黑洞、>2 GiB 历史回归未执行。无活动 CLI 的自动重连不替代“运行中 CLI 存活且输入不重放”的公网验收。交付前执行文档/diff 检查；二进制策略只检查空暂存区（checked=0），不当成全部未提交内容或远端历史审计通过。
