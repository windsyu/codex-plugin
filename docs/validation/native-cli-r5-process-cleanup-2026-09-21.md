# R5 增量：启动终端挂断与验收浏览器回收

日期：2026-09-21。状态只在[实施计划](../v2-implementation-plan.md)维护；本文不代表 R5 整片通过。分支 `codex/native-cli-live-workbench`，HEAD `9301724` 加当前工作树；未提交或发布。

## 1. 问题与修复

用户核查后台进程时发现旧 App Server 和本项目的无界面测试 Chrome 遗留，已按授权关闭。旧 App Server 属于待退役的旧运行路径；清理存量进程不代表旧控制代码已退役，也不能据此断言它们来自新工作台。

本轮在 macOS 26.5.2 / arm64、安装版 CLI 0.155.1、系统 Chrome 153.0.8010.50 上另做了两个独立回归：

1. **启动终端挂断。** [正式启动器回归](../../src/workbench/proxy/tests/native_cli/launcher.rs)依次发送 SIGINT、SIGTERM、SIGHUP。修复前第三项报 `signal 1 bypassed graceful shutdown`；这是未走正常退出的证据，不声称每次 SIGHUP 都会留下 CLI。[启动器](../../src/bin/codex-view.rs)现在在创建 CLI 前注册 SIGHUP，并与原有两个信号进入同一清理路径。
2. **测试控制进程退出。** [浏览器回归](../../src/workbench/probe_process_tests.rs)先使用既有 `kill_on_drop(true)` 方式，观察到 `dropping ready probe left Chrome alive`；失败清理器定向结束了本次测试浏览器组。Node 退出并不等于独立 Chrome 进程组退出，不能仅检查 Node 的退出码。

浏览器修复由两层配合：

- [Rust 探针所有者](../../src/workbench/probe_process.rs)在正常返回、超时或 panic 释放时，关闭控制管道并通知 Node，等待回收后再返回；最长等待 20 秒，超时有安全诊断和 Node 强制回收兜底。
- [Node 生命周期助手](../../web/e2e/browser-lifecycle.cjs)通过 Playwright 公共 `BrowserServer` API 持有 Chrome。退出发生于启动途中时，等待启动结果再清理；正常关闭等待 3 秒，Chrome 不响应则定向 `kill()` 并等待其退出，不先 `process.exit()` 丢下浏览器。

助手使用随机端口的 loopback 连接；私有连接地址仅在内存传递，不写参数、stdout 或报告。R0–R5 浏览器探针及 Rust 调用位置采用同一机制，R0 性能、R5 真实模型示例也使用相同所有者；该代码仅参与测试/示例，不加入工作台生产运行依赖。后续复核发现早期 `examples/r1-debug.rs` 的 Rust 调用位置漏接，已在[终端工具独立化增量](native-cli-r5-terminal-extraction-2026-09-21.md)中补齐；此前测试结果不能视为该示例已接入的证据。

## 2. 已验证行为

| 场景 | 断言及结果 |
| --- | --- |
| 已就绪的正式工作台收到 SIGINT / SIGTERM / SIGHUP | 三次均正常退出，所属 CLI PID 消失，网页监听关闭，私有入口删除 |
| 网页停止、重复停止、原生退出、回退普通 CLI | 既有正式入口回归通过；停止后仍可阅读，刷新不换 PID，无关进程保留 |
| 探针已就绪后被释放 | Node、Chrome 主进程及同组辅助进程全部退出 |
| Rust 仅关闭控制管道 | Node 收到 EOF 后回收 Chrome，全组退出 |
| Chrome 仍在启动途中 | 合成启动包装器延迟 1 秒，退出不丢失迟到的浏览器句柄；全组退出 |
| Chrome 主进程无响应 | 向本次测试 Chrome 发 SIGSTOP，正常关闭超时后强制回收全组；旁侧无关进程仍活着 |
| 实时页面与正式原生交互 | 正文中间态、最终替换、未知类型、快照缺口恢复、刷新、原生中文两轮输入通过，页面错误为 0 |
| 设置面板与探针控制管道 | JSON 保存、外部冲突、清理/保留期限、三种视口与终端保持通过；Rust 与 Node 的双向测试指令仍可用 |
| 性能探针兼容 | 当前页面 `small` 单例 400 个客户端/正文样本，CDP/DOM/Paint 测量完成，页面错误与观察丢失均为 0 |

Chrome 回归实际检查进程组成员，覆盖辅助进程；不是仅从 Node 或浏览器主 PID 推断。测试各自设置临时 HOME、USERPROFILE、CODEX_HOME，只打开合成页面或 about:blank，不读取私人会话，不下载浏览器。失败路径的定向清理同样受测。

## 3. 复现与限制

```bash
cargo build --locked --offline --bin codex-view --examples
cargo test --locked --offline --lib dropped_probe_cleans_chrome_after_ready_during_launch_and_when_unresponsive -- --ignored --nocapture
cargo test --locked --offline --lib workbench::proxy::tests::native_cli::launcher:: -- --ignored --nocapture --test-threads=1
cargo test --locked --offline --lib workbench::web::tests::browser:: -- --ignored --nocapture --test-threads=1
cargo test --locked --offline --lib browser_settings_expand_save_conflict_and_collapse_keep_reading_and_terminal -- --ignored --nocapture
cargo test --locked --offline --lib -- --test-threads=4
target/debug/examples/r0-latency --product-page --case small
cargo fmt --all -- --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo build --locked --offline --all-targets
```

结果：工作台库 232 通过、31 个需要显式环境的测试 ignored；本轮另外执行上述 7 项 CLI/Chrome 测试，全部通过。fmt、Clippy 全 targets、全 targets 构建、22 份浏览器脚本语法、5 份文档的 202 个本地链接/锚点及 diff 空白检查通过。性能探针本次使用 headless Chrome，仅用于验证新所有者与控制/CDP 流程，不能替代此前有界故障矩阵和可见窗口的性能验收，也不用于比较性能提升。最终进程盘点没有本项目的工作台、Node 浏览器探针或遗留测试 Chrome；未启动常驻调试服务。

本轮尚不证明 SIGKILL、系统崩溃、Node 自身无法处理退出信号时的完整回收，也不证明所有原生工具自行脱离 CLI 进程组后的回收。Chrome 无响应与 Node 无响应是不同故障，不混为一项通过。没有修改官方 CLI、扫描并终止其他用户会话，或增加全局进程清理器。

没有新增用户配置、数据库 migration 或接口字段。真实中文输入法、手机设备范围与旧控制退役仍按实施计划继续；本增量不能替代这些验收。
