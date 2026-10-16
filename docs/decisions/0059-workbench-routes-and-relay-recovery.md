# ADR 0059：独立功能页面与中转断线恢复

- Status: Accepted
- Date: 2026-10-14

## Context

用户要求美化前端、将不同功能拆到独立页面路由，并加固电脑与中转连接，再在指定 ECS 上用独立浏览器会话验证公网原生 CLI 输入和消息响应。此前 RQ11 / ADR 0048 的“设置同页、无独立路由”决定被此次明确要求覆盖；既有验证仍只证明当时版本。

已有账号和单端口中央前端沿用 ADR 0058。页面授权、电脑归属、原生输入去重、CLI 生命周期和同源 XSS 边界不能因页面拆分放宽。仅能打开网页不等于完成远程控制验收。

## Decision

1. 门户使用 `/login`、`/computers`、`/authorize`、`/account/security`、`/admin/users`，根路径按登录和首次改密状态导航。兼容旧 `/#bind=...`，设备码只在当前标签保存，不留在导航 URL 中；明确退出清除授权意图。
2. 工作台使用 `/` 概览、`/history` 历史、`/launch` 启动、`/settings` 配置和仅本机可用的 `/relay` 公网接入。Run 使用 `/runs/{runId}/{chat|terminal|files|search|git|history|usage|settings|access}`。旧 `/?run=...` 保持兼容。
3. 本机使用根路径；单端口中转在上述工作台路径前加 `/g/{gatewayId}`。服务端只接受精确页面白名单，不以通用 SPA fallback 覆盖未知 API、静态资源或任意路径。独立 Run 阅读入口也提供当前 epoch 的 scoped API，完整文档、配对、读取和终端链路保持一致；路径重写仍复用既有 Host/Origin、Cookie 和 ingress 检查，不能扩大到其他 Run 或全局入口。工作台 HTML、JS、CSS 继续由中转发布。
4. 浏览器采用轻量显式路由和 history/popstate；同一 Run 的功能切换保留同一个终端实例和 CLI。每次 HTTP/SSE/WS 与新标签导航继续固定原页面授权，不依据后来登录的 Cookie 补发权限。页面 URL 不成为启动命令，也不重放输入。
5. 电脑控制通道使用有界连接/写入/续租超时和有界退避；稳定连接后恢复快速重连，失败重试带抖动。控制授权过期优先于新请求受理。旧 epoch 断开时关闭远程流与输入许可，但不结束本机 CLI。认证拒绝停止重连，要求用户重新认证。
6. 自动恢复只重建连接和读取状态，不重试业务写入、不重发终端按键、不自动恢复 CLI。输入 ACK 丢失保持“未确认”提示。浏览器关闭后的空闲 SSE 也须及时回收。
7. 新路由只是兼容的展示变化，不修改 Application/启动/终端 wire contract，仍使用 `WORKBENCH_API_VERSION=1`；若后续改变该契约则必须配套提升版本，不回退加载电脑端脚本。

### 授权期限与睡眠

连接握手和远程输入许可的期限使用包含系统睡眠的连续单调时钟：Apple `CLOCK_MONOTONIC_RAW`（映射到 `mach_continuous_time`），Linux/Android `CLOCK_BOOTTIME`。普通 `Instant` / Tokio 定时器只用于调度、唤醒及重试退避，不能单独证明授权仍有效；控制帧受理、续租和 PTY 实际执行点均再次检查连续期限。时钟失败、倒退、非法返回或期限溢出一律撤销，不回退到 wall clock；未经验证的平台只拒绝 leased 远程许可，本机未租约许可不受影响。依据见 [Apple Libc 实现](https://github.com/apple-oss-distributions/Libc/blob/main/gen/clock_gettime.c)及 [Linux timekeeping](https://www.kernel.org/doc/html/latest/core-api/timekeeping.html)。合成连续时钟 oracle 不替代真实硬件睡眠验收。

### 首次连接状态

schema 2 的电脑 `workbench_api_version=0` 表示登记后尚未收到 Hello，门户显示“等待连接”，每 4 秒刷新，不能提前宣称不兼容。显式发送 API 0 的旧 Hello 在存储中标记为 `u32::MAX`，与未知状态区分；断开后仍提示升级，不能回退加载电脑脚本。已知版本严格匹配 API 1，认证代际失效优先显示需要重新登录。

## Alternatives

- 只给旧面板改 URL：不能满足功能独立页面的要求，也难以支持直达和恢复。
- 每次功能切换整页刷新：会反复建立终端、改变输入接管体验；保留普通链接以支持新标签，但同 Run 内使用轻量导航。
- 无限快速重连或重试写入：会造成连接风暴和重复操作；选择有界恢复并保持输入不重放。
- 增加端口、V3 消息桥或网页 Composer：超出本轮范围，不采用。

## Consequences

需要同时更新本机文档路由、中转中央页面、协议路径白名单、导航和历史恢复测试。设置 URL 变化，但配置格式、SQLite schema 2、原生 Codex 配置和数据格式不变。旧域名模式保持显式配置和独立回归。

发布顺序为前端构建、配套 Mac/Linux 二进制、自动检查、系统 Chrome 回归，再备份并升级指定测试中转。部署不改业务 443、业务容器、安全组或防火墙；先确认本机没有活动任务才重启测试电脑。

此次数据库结构仍为 schema 2。升级失败时先保全现场，结构与完整性核验通过后只回退二进制和启动配置，保留当前数据库中的并发登录、改密和撤销；不能自动恢复旧快照。跨 schema 或损坏数据的恢复需另行决策。

公网验收使用独立浏览器会话模拟手机，记录实际 CLI 版本、进程、输入次数、模型响应、SSE、断线和服务重启后的恢复。匿名/窄屏 Chrome 的结果不冒充真实手机硬件与移动网络验收。用户登录密码未知时不得提取密码库或重置已有账号，需要用户自行登录或明确提供测试账号。
