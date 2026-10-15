# 后台服务与远端目录浏览验证（2026-10-15）

## 范围与环境

实现分支 `codex/workbench-background-browse`，起点 `79f4c046482ab7561cd4f269d39ad5166bc8048d`；验证其增量工作树，未据此宣称发布或公网部署。契约见 [ADR 0063](../decisions/0063-background-service-directory-browser.md)，需求与状态见 [实施计划](../v2-implementation-plan.md)。

环境：macOS 26.5.2 / Apple Silicon、Rust 1.95.0、Node 22.23.2、系统 Chrome 154.0.8037.100。自动化使用独立 HOME、USERPROFILE、CODEX_HOME、合成账号与 CLI。新版启动前已核实旧试用 PID 35687 的用户、可执行文件和目录归属，正常 SIGTERM 并确认退出；没有批量按进程名结束。原工作树未提交的部署记录保留。

## 标准检查与构建

- 标准 `check --component all --offline` 的工具与前端部分：99 项工具测试、318 项前端测试、TypeScript、Web 构建通过。证据：`target/artifacts/96cfbe3e-9851-4f4b-9d58-bd03c13f9a29`。该次任务因随后 Clippy 错误标为 failure，不能整体称通过。
- 修复 Clippy 布尔表达式及多余借用后，用同一标准组件配方继续执行：全 workspace Rust 常规测试 583 项通过、57 项专项按条件 ignored；fmt、Clippy `-D warnings`、工作台两个 binary 和独立中转 debug/release 构建通过。证据：`target/artifacts/1b37d785-5992-4d2e-b347-b9877fb65f50`。
- 最后抽取可注入浏览器启动器的注册引导函数，追加 binary 测试（3/3）、配置锁回归（1/1）、全 workspace Clippy 和工作台 debug/release 增量构建。证据：`target/artifacts/c7d26ec8-0085-4d01-a050-0277bce9c8ab`。已有全量证据与此增量共同覆盖最终状态，没有跳过失败或降低检查门槛。

注册引导测试覆盖首次未绑定、有效登记、手动关闭、临时离线、明确失效、非交互、`--no-open` 和浏览器失败不阻断服务；已有连接器测试覆盖批准持久化、取消与拒绝凭据。浏览器失败采用注入，不声称真实 OS 故障实测。

## 目录、身份与 Chrome

12 项目录测试覆盖中文/空格、隐藏项、筛选、符号链接、无权限/消失、目录替换、分页、大目录与字节/扫描上限、恶意名称、快照过期、请求取消后扫描许可保留。APFS 不允许创建非法 UTF-8 名称，该输入直接注入生产名称处理边界；真实扫描另验证控制字符名称被省略并标记不完整，保留了负向断言。

Application 集成测试覆盖注入主目录、私有控制能力、错误 Origin/Host、旧实例、重复 Content-Type、关闭后停止接纳启动和所属 CLI 回收。PairedRun cookie 无权访问目录/控制；双账号/双电脑合成中转验证连接电脑主目录、跨账号拒绝、旧实例冲突；控制与私有 connect POST 不进入中转白名单。OS 权限错误返回 422，避免误当账号撤销。

系统 Chrome 常规 E2E 10/10：`target/artifacts/572b6f49-fb87-4a95-9ec7-52db11cd4aa1`。目录专项在 1280px 桌面和 320px 手机视口验证模态范围、键盘焦点约束/恢复、权限错误、隐藏项、选择后校验但零启动、授权失效清除；零 page error。截图/报告：`target/artifacts/1f3db62f-96c5-44bd-bec8-af6d7425b3a9`。UI 专项使用模拟接口；权限与实际转发另由 Rust 中转集成测试覆盖，不将模拟 UI 当成公网实测。

## 真实 LaunchAgent

复现：标准构建工作台后执行 `node scripts/verify-background-service.mjs`。仅对任务独立 HOME 创建 LaunchAgent，结束时停止并移除测试登记。成功报告：`target/artifacts/275a03d4-4b5e-4815-8d40-55385492782b/report.json`。

1. 后台启动零项目；非交互仅打印注册说明；命令退出后服务存活；重复 start 保持实例与显式默认值。
2. 登录自启开关不改变当前 PID；stop 保留偏好；再次手动启动成功。
3. 已加载任务正常退出后可再次 start。首次实测发现 8 秒 launchctl 客户端超时不足以覆盖 30 秒节流，调整 kickstart/bootout 为 40 秒预算后复验通过。
4. 对私有状态确认的本任务实例模拟异常退出，launchd 限频重启工作台；两个合成 CLI 都退出，重启后零项目，没有恢复或输入重放。
5. 再次启动两个项目，破坏当前 JSON 配置后 status/stop 仍可定位作用域，完整回收所属 CLI 并退出。
6. 前台实例存在时 start 明确拒绝接管；私有 stop 能正常结束前台实例。

18 项服务单元测试及安装工具测试覆盖全部作用域先预检再删除、首次登记竞争、安装保留锁、修改/符号链接/硬链接保护、旧版本指导、跨安装/原生目录/参数冲突。独立审核发现的安装竞争和自启沿用错误身份均已修复。

并发 CLI fork 还会在 exec 前延长继承的配置 flock 寿命。确定性 fork/pipe 回归先复现失败，再以 guard Drop 显式解锁修复；父进程写入唤醒字节并回收子进程，不依赖其他 fork 关闭 pipe。独立审核复核了修复。

## 本机试用与限制

自动化结束后，使用最终 debug 二进制在真实用户作用域执行交互式 `start`：运行方式为后台、活动项目 0、自启关闭、未登记且接入关闭。注册提示出现，系统浏览器启动器成功返回；需要用户在接入页选择可信中转并完成登录批准，没有自动填写账号或授权。

没有数据库 schema、协议版本或原生 CLI 修改；目录能力字段缺失时保留手动输入。真实注销/重新登录自启、睡眠/唤醒、真实手机和新版公网部署未执行；服务不保证注销、睡眠或关机期间在线。本次本机后台实例保持运行供试用。
