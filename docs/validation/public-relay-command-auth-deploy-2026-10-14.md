# 命令认证配套中转更新（2026-10-14）

## 范围与来源

用户确认本机试运行成功后，明确要求更新指定 ECS `i-bp16nvvpdk423m48yewl` 的既有测试部署。本次从 `codex/public-relay`、基础 commit `fa094c23b3742a6bb6cb475c7ac924fd7357582f` 加当前未提交增量构建 Linux 中转及中央前端；不是正式 release，没有提交、推送或变更 V3。

命令认证改造及安装版合成验证见[原生命令认证记录](native-cli-command-auth-2026-10-14.md)。认证命令仍在所属电脑的官方 CLI 执行，不迁移到中转服务器，不上传本机原生配置、认证命令、token 或历史。本次只更新服务器配套产物并验证连接，不发送新的真实模型消息。

## 构建与升级

使用原已验证的交叉构建包装器，Rust 1.95.0、Node 22.23.2、Zig 0.14.1；先生成 Web，再以 Cargo `--locked --offline --release` 构建 `x86_64-unknown-linux-musl` 中转。构建 artifact `36ee76b5-66a8-4e73-ada9-1f032fbf3361`，292 项许可包括四类静态运行库通知。上传候选为静态 x86-64 ELF，无 PT_INTERP/PT_DYNAMIC。

原始 tar 检查发现 macOS provenance 扩展元数据，未上传该包；以 `--no-xattrs --no-acls --no-mac-metadata` 生成独立 clean 包后重新验证。最终 3864 成员、3337 普通文件；无重复、路径穿越、链接、特殊文件或扩展元数据，成员字节与分发目录一致。

```text
binary    7815e49c3f7b3d15d78d46a9604d90c27a3951aa3fd68e9fba04712c71905b48
archive   9fdd6c95f46652443bdba9de3cc2da531f849375e4c5acf8d90e53eef1d785c4
manifest  d445f4ecc70d29202e110b3a4c887e624c3acbe70020235849004a5c0c73818c
checksums 1499ffca6e162ce82bb3a30c33d3f38e25662cb0f5481050ba99e8ab3d286ace
```

复用[上次升级](public-relay-routes-recovery-2026-10-14.md)已独立审查的 schema 2 升级/保护脚本，只调整旧产物路径与 SHA-256 两个常量；12 项合成部署保护回归及 shell 语法检查通过。本轮尝试的独立 Astra 复审因模型容量不可用，未形成新审查意见；不将执行者复核称为独立复审。

远端预检 artifact `21f98790-796d-41c6-a010-9cbe0c7629aa`：实际 ECS 身份、当前产物与参数、新包全部文件校验和/许可、schema/完整性、既有公网 TLS 路由均通过。apply artifact `e2ddfeb2-f55c-45ea-bac1-9c19e259eaff` 返回 0；首次 readiness 请求短暂 connection refused，随后有界就绪检查通过，没有触发回退。

- 新产物：`/opt/codex-relay-trial/releases/36ee76b5-66a8-4e73-ada9-1f032fbf3361/relay-upgrade`。
- 原产物 `d755a016-cfc4-4090-851d-b5be5b228f74` 完整保留；仅切换专用服务 drop-in 的二进制路径。
- 私有备份：`/opt/codex-relay-trial/backups/36ee76b5-66a8-4e73-ada9-1f032fbf3361-schema2`；加密/HMAC 归档及独立密钥都留在服务器私有目录。
- 备份实际验证 HMAC 后解密到独立 `restore-check`；一致性 SQLite 快照、配置和旧二进制字节一致，恢复数据完整性/外键及账号/电脑保护字段比对通过；没有覆盖在线库。
- schema 2 结构不变，3 个账号和 1 台电脑保留。升级前后密码、角色、禁用/强制改密状态、认证代际、电脑归属与凭据保护字段相同；没有密码重置或凭据初始化。

失败策略继续只回退程序和启动配置、保留当前数据库，不自动恢复旧快照。没有 TLS reload、安全组、防火墙、端口、原生 CLI 升级或其他服务修改。

## 实际运行检查

专用 `codex-relayd-ip-trial.service` 为 active/running，新 PID 1399753，NRestarts=0。运行中二进制真实路径与 SHA-256 匹配。限额仍为 MemoryLimit 402653184、CPUQuotaPerSecUSec 350ms、TasksMax 64；后端只监听 loopback 8787，公网复用原 HTTPS 9443。

公网资源检查 artifact `6f6a7e60-6765-497e-8a71-c9e02a0de91d`：门户 200，匿名 account/gateways/admin-users 均 401；全部 6 份工作台构建资源返回 200，并与当前 `web/dist/assets` 逐字节相同。TLS 校验开启并通过，没有以网页 200 冒充模型认证。

系统 Chrome 上原本机测试页面显示“已连接”、账号 `admin`、电脑“本机临时测试电脑”；没有重新登录或修改本机凭据。原本机工作台 PID 69596 和 `http://127.0.0.1:62429/` 入口保持。这里只确认电脑控制通道恢复，不推导输入重放、同一 CLI PID 或完整公网终端验收。

原 8 个业务容器和专用 TLS 容器的 ID、启动时间、重启次数逐项未变；原 443 入口仍为 200、TLS 校验为 0。专用 unit 和环境文件与升级前备份字节一致，除预期二进制路径外没有配置变化。

## 剩余范围

用户报告本机试运行成功，不替代独立的真实手机/移动网络、多电脑公网并行、命令 token 真实刷新或公网输入不重放验收。本次没有读取密码库、复制浏览器 Cookie、执行认证命令或发送真实模型请求；新公网消息仍由用户在门户登录后继续试用。

本轮产品源码未新增修改，只补齐部署记录、验证索引与实施计划说明；文档检查和 `git diff --check` 在交付前执行。完整标准检查复用命令认证实现后的 success artifact `b3fe8e72-b8ce-4230-a9fe-2fb21493a04e`，本轮 Linux 配套构建另列，不冒充重跑全套测试。阶段状态仍以[V2 实施计划](../v2-implementation-plan.md)为准。
