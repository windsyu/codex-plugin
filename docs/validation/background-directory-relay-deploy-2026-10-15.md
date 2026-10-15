# 后台服务与目录浏览配套中转部署（2026-10-15）

[验证索引](README.md) · [本机实现验证](background-service-directory-browser-2026-10-15.md) · [组件部署](../guides/component-deployment.md)

## 来源与范围

用户授权通过 Workbench CLI 更新 ECS `i-bp16nvvpdk423m48yewl` 的现有中转。部署源码为干净的 `aa459c25d4b3de17b4b71764cf1be521ba0ae93b`，分支 `codex/workbench-background-browse`；本记录在部署后补充，不属于当时的源码树。未执行 Git push、PR、正式 release 或其他分支合并。

此次将远端目录浏览前端和精确的 `POST /workbench/v1/application/directories` 中转白名单一起更新。本机生命周期私有控制未加入白名单。`relay/src/store.rs` 相对部署前源码 `79f4c04` 未变化，无数据库 schema 或协议版本迁移。实现的标准检查与 Chrome、LaunchAgent 结果见[本机验证](background-service-directory-browser-2026-10-15.md)，本轮不重复将其计为 Linux 实测。

## Linux 构建与分发

使用 Rust 1.95.0、Node 22.23.2、已校验 Zig 0.14.1 和独立 Cargo 目录，通过标准 `node scripts/dev.mjs build --component relay --release --offline` 先重建前端，再交叉编译 `x86_64-unknown-linux-musl`。受管分发任务 `7c78f8ff-ab7d-42da-ba60-45c63324b9d4`、标准构建任务 `385d1898-3fe7-417c-a533-c4e4539c3a02` 均成功。

产物是静态 x86-64 ELF，无 PT_INTERP、PT_DYNAMIC 或动态依赖。归档的 729 个成员、453 个文件逐项通过路径、类型、mode、字节、完整文件集合与校验和验证；不含链接、特殊文件或 AppleDouble。分发只包含 relay、来源/锁文件及许可材料，没有本机连接器、用户数据或编译工具。249 项许可覆盖 196 项 Cargo、49 项 npm 和 4 组实际静态运行库通知；运行库保留 32 份原始通知。

| 产物 | SHA-256 |
| --- | --- |
| Linux codex-relayd | `288b1fb85d69d2232dfff50e78360ba0bd9fa5ea29f493e0974f03a19c411ffd` |
| Linux archive | `0229a95d015be644aaedb692384a6b6a608cc11fc59b958315615fdb2ef52c16` |
| manifest | `9cab3b1fe739d10945e7b75838d9f9b64f6ad930381b2f2f6d5a40fa6727f066` |
| CHECKSUMS | `29bc04ac5f32fc21c81c4dbd2f674f7195b4fe74a28514bb58b86c8c3ef97d8a` |

## 备份与切换

先核实目标实例、旧程序路径/摘要、精确 systemd 参数、专用用户、私有数据权限、schema 3 完整性及既有 TLS。升级前现场为 2 个账号、0 台登记电脑，未按旧记录修改现场。旧程序摘要为 `78ae1e8a3e784f44f527784433f450def0ff9ece9f4abca65bf307c68cb368af`。

复用并适配既有部署守卫，独立审核未发现阻止部署的缺陷；Shell/Node 语法、Python 3.6 兼容、10 项合成守卫检查及新包检查通过。暂存目录采用独立 UUID；上传后重新验证所有文件摘要、权限与归档边界，远端包检查保留 root 所有权要求。在停服前，以服务用户实际运行新 Linux 程序 `--help`，验证可执行性和路径可访问性。

只停止 `codex-relayd-ip-trial.service`，确认 PID 与后端监听退出，保留旧配置、分发及整个私有数据目录，并生成 SQLite 一致性副本。备份使用独立私有密钥加密、HMAC 校验，实际恢复到新的私有目录，比较 schema、账号与电脑保护字段。随后仅原子修改专用 drop-in 的程序路径并启动；没有恢复在线旧数据库、重置密码或重建账号。

- 新分发：`/opt/codex-relay-trial/releases/cf0232f5-812b-455a-9135-b3d385d4bd12/relay-upgrade`。
- 私有备份：`/opt/codex-relay-trial/backups/cf0232f5-812b-455a-9135-b3d385d4bd12-schema3-background-browse`。
- 暂存任务 `930691b1-e952-450f-9d90-86ebd3a9dce3`、preflight `2268a33b-b813-4e63-8013-9dce5278e030`、apply `05119c72-ab80-4022-9bd9-0198c568f443` 均成功。启动轮询首次连接尚未就绪，随后全部健康门槛通过，未触发失败恢复。
- 服务新 PID `1674037`，active/running，NRestarts=0；`/proc` 程序路径与摘要匹配新包。仅监听 `127.0.0.1:8787`；CPU 350ms/s、MemoryLimit 384 MiB、TasksMax 64 保持。现场使用旧 cgroup 内存属性，不以 MemoryMax 的 infinity 误判限额。
- 切换前后 schema 3、完整性、外键检查、账号与电脑保护状态一致。原有 8 个业务容器及专用 TLS 容器的 ID、启动时间、重启次数与健康状态逐项一致。443 入口及 9443 门户均 200，TLS 校验结果为 0。

切换后失败处理会停服并保全当前数据，不自动恢复旧快照。本次与旧版 schema/数据库契约兼容；人工回退只应在确认兼容后修改程序/启动配置并保留最新数据库，不能用本次快照覆盖此后账号、密码、撤销或电脑登记变化。SIGKILL、断电及人为并发配置变更仍须人工核对。

## 公网与实际电脑验证

公网任务 `915ea7c8-74f8-41a3-af17-5f9f7b33d8a9` 的 17 项检查全部通过：6 个门户文档 200、3 个匿名账号/电脑/管理 API 401、2 个门户资源与 6 个工作台资源逐字节匹配；全程启用 TLS 校验。升级前后脱敏状态任务分别为 `1b929283-867b-409b-bf71-5f300b7a75d5`、`5f38be9a-8769-4995-84cd-d1f9faa8b729`。

系统 Chrome 打开 `https://47.97.44.196:9443/computers`，已有登录继续有效。切换完成后的检查窗口内尚无电脑；随后用户完成本机登记并启动项目。本机只读 status 确认原后台 PID `66390` 保持运行，中转 registered/enabled/online，活动项目 1、自启关闭。未由 agent 代填凭据、批准登记、启动或停止项目，也未发送模型输入。

后续只读现场快照任务 `05dae7b5-9012-4340-962d-3491797654fe` 为 1 个账号、1 台电脑，schema 3、完整性和外键检查仍通过，relay PID 与程序摘要保持。该快照发生在上述切换保护比较完成后的用户试用期间；没有用部署前备份覆盖后续账号或电脑状态。

在独立 Chrome 标签通过门户打开已在线电脑，进入“启动项目 → 浏览电脑目录”，真实公网链路返回本机主目录、磁盘入口、面包屑和子目录；名称筛选实际成功。没有选择目录或创建额外 CLI。验收截图保留在受管准备任务 `cf0232f5-812b-455a-9135-b3d385d4bd12/remote-directory-browser.png`，未将包含实际用户路径的截图或页面 grant 提交 Git。目录浏览标签保留供试用。

真实手机/移动网络、完整公网模型消息、真实登录自启和睡眠恢复仍未验收。当前本机历史首页仍显示历史索引暂不可用；本次没有删除或重建私人历史，也未将该既有问题计为修复。
