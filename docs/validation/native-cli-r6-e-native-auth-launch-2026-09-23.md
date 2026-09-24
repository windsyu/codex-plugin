# R6-E：新目录启动与原生 API Key 兼容修复

日期：2026-09-23。针对首页选择新目录后提示“Codex 未能启动”而无法进入 V2 的反馈。本次修复启动兼容与错误提示，不实施 R6-F。

## 1. 根因与事实边界

只读检查发现当前原生配置使用 `custom`、Responses、显式 provider bearer 和 `requires_openai_auth=true`，File 认证是 API Key、没有 ChatGPT tokens。没有输出凭证值或修改真实配置。项目目录不是此次拒绝的原因：旧适配器直接要求该标志为 false；Application 又把具体失败统一变为 `native_launch_failed`。

新增回归 `explicit_bearer_accepts_native_api_key_auth_without_changing_credentials` 在修复前失败，拒绝原因为 `OpenAI login capture is not validated for this profile`。最初另有一次构建被 Web 产物新鲜度检查挡住，先完成前端构建后才得到上述功能回归失败，二者不混为一项证据。

官方参考仓库实际 commit 为 `633ab199cfd724aa78013c006b27a2b3d049fc3b`，只读核对：

- `codex-rs/model-provider/src/auth.rs`：`resolve_provider_auth` 先选择显式 provider bearer，再处理原生认证；静态 bearer 优先于原生 API Key。
- `codex-rs/core/config.schema.json`：`requires_openai_auth` 包含 API Key 和 ChatGPT 两种原生认证，并不表示必须 ChatGPT 登录。
- `codex-rs/core/tests/suite/external_auth.rs`：已有显式 bearer 优先级及不发送 ChatGPT 账户 header 的验证；该官方用例本身使用 false，不能替代本次 true 的安装版回归。

本机 CLI `0.155.1` 的版本通过隔离目录实测。参考源码与安装二进制的身份未假定为一致，以以下安装版实验限定本次支持范围。决策见 [ADR 0054](../decisions/0054-native-custom-bearer-auth.md)。

## 2. 修复范围

- `src/workbench/launch/profile.rs` 接受显式 true/false，仍要求原有受测路由和静态 bearer；既有管理、ChatGPT tokens、keyring/auto、环境/命令认证及 WS 限制保持。
- `src/workbench/config_sources.rs` 有界读取原生认证快照，仅将 API Key 加入观察副本脱敏，不参与 Authorization 构造。快照无 Debug/Serialize；检查创建、删除、字节变化以及 API Key 切换至 cloud tokens。
- `src/workbench/launch.rs` 在调用方最终校验后、spawn 前复核配置和认证快照；对应回归确认变化后没有创建 CLI。该检查不是文件锁，不消除最后检查之后或运行期间的外部编辑竞态。
- `launch/failure.rs`、Application 和 `web/src/workbench/launchMessages.ts` 传递固定错误码，共用中文提示。操作结果、首页与直接启动握手不序列化任意错误链；未知错误及网络异常使用固定回退说明。
- 补充启动、Application、认证、前端错误映射与真实 CLI/Chrome 回归。所有 fixture 使用合成值，未新增配置字段或迁移。

## 3. 验证结果

| 检查 | 结果 |
| --- | --- |
| `workbench::launch` | 14 passed，包含先失败后通过的 true 组合、凭证变更阻止 spawn、true/false 的未验证来源负向覆盖 |
| `workbench::application` | 22 passed、2 ignored；包括异目录 V2 端点与安全错误码；忽略项不算通过 |
| `workbench::config_sources` | 6 passed，包含快照变化、无效/超限输入与诊断排除秘密 |
| `useLaunch` / `HistoryHome` 前端测试 | 30 passed，涵盖具体原因、未知码、原型键及异常正文回退 |
| 前端类型检查与 Vite 构建 | 通过 |
| 安装版 CLI + 系统 Chrome | 原有 false 流程与新增 true + 独立 API Key 流程分别通过；Chrome `153.0.8010.53` |
| Clippy、Cargo fmt、JS 语法、Git diff 检查及 debug 构建 | 通过 |

真实 CLI 回归启动最新 `target/debug/codex-view`，HOME、USERPROFILE、CODEX_HOME 均指向临时目录；模型只访问本机合成上游。从临时父目录启动应用，再从首页输入另一个含中文与空格的项目绝对路径。测试不操作桌面鼠标、不启动原生目录窗口，不写真实 `~/.codex` 或 `~/.codex-web`。

新增组合验证：

1. 首页初始没有 Run；确认项目后创建 V2 工作台，Run 的项目和 URL 与所选目录一致。
2. CLI 请求继续发送显式 provider bearer，不发送独立的原生 API Key，也没有 ChatGPT-Account-ID。
3. SSE 跨多个 chunk 回显两种合成秘密，流式中间态在结束标记到来前可见；聊天和 live snapshot 中两种秘密均已脱敏。
4. 文件面板打开所选项目内的合成文件，正文正确。
5. 停止 CLI、回首页、明确“继续此会话”，获得新 Run/PID，保持相同原生 Thread；新输入前没有自动模型请求，第二轮保留前文。
6. `auth.json` 字节未变化；原生模型与 provider 配置未变化。CLI 可以按原生流程增加项目 trust，故不声称整个 config.toml 字节不变。
7. 两个 Run 的观察 journal 存在并含测试正文标记，不含两种合成秘密；浏览器页面错误为 0，应用正常退出并清理 entry。

首次新增端到端断言在“中间态时序”失败：测试标记的末尾 `D` 是通用 `data:` 脱敏前缀，按既有规则暂存到后续字节到来。补上明确分隔符后，原有生产脱敏逻辑不变，真实中间态、snapshot 与保存记录检查全部通过。保留细分阶段码用于后续失败定位，不输出终端正文或凭证。

```sh
npm run test --prefix web -- src/workbench/useLaunch.test.tsx src/workbench/HistoryHome.test.tsx
# 在 web/ 下执行：npx tsc --noEmit
npm run build --prefix web
cargo build --offline --bin codex-view
cargo test --offline --lib workbench::launch -- --test-threads=1
cargo test --offline --lib workbench::application -- --test-threads=2
cargo test --offline --lib workbench::config_sources
cargo test --offline --lib product_homepage_launches_ -- --ignored --nocapture --test-threads=1
cargo clippy --offline --all-targets -- -D warnings
cargo fmt --all -- --check
node --check web/e2e/r6-auth-launch-probe.cjs
git diff --check
```

需要本机端口和 PTY 的测试在获得权限的执行环境运行。首次组合执行中的原有 false 用例已通过；新增 true 用例修正测试标记后单独重跑通过。日志位于 `/tmp/codex-new-directory-*.log`，不提交环境数据。独立 GPT-6 只读审查未发现阻塞问题，主代理完成集成和安装版验证。

收尾只读进程检查未发现 codex-view、R6 浏览器探针或 SkyComputerUseService 残留；已有的其他 Codex 进程未操作。相关 7 份文档的本地链接与代码块检查通过。

## 4. 交付与限制

已重建 `target/debug/codex-view`；前端内嵌在二进制中，试用新版需退出旧实例再启动，重复启动可能复用仍在运行的旧服务。没有替用户停止活动项目。

本次没有迁移、配置格式变化、用户凭证写入或真实项目模型调用；不宣称已在用户实际项目上完成真人复验。分支仍为 `codex/native-cli-live-workbench`，累积 R6 修改未提交、未推送。继续停留 R6-E，下一步由用户试用此修复；R6-F 仍为 Pending。
