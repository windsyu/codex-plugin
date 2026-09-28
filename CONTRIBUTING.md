# 参与贡献

欢迎提交可复现的问题、文档修正和小而完整的改动。开始前阅读[文档导航](docs/README.md)、[支持范围](docs/codex-native-cli-workbench-support.md)和 [AGENTS.md](AGENTS.md)。产品实施状态只在 [V2 实施计划](docs/v2-implementation-plan.md)维护。

## 准备环境

开发和 CI 使用 Rust **1.95.0**（含 rustfmt、Clippy）与 Node **22.23.2**，版本分别由 `rust-toolchain.toml`、`.nvmrc` 固定。这是验证基线，不是最低支持 Rust 版本声明。macOS 构建还需要 Apple Command Line Tools；浏览器测试使用已有 Google Chrome，不下载 Playwright 浏览器。官方 Codex CLI 仅为原生专项测试所需，不是常规检查前提。

```sh
node scripts/dev.mjs doctor
node scripts/dev.mjs bootstrap
node scripts/dev.mjs build
node scripts/dev.mjs check
node scripts/dev.mjs e2e
```

`bootstrap` 使用 `npm ci`，Rust 命令使用 `--locked`。已有完整本地依赖时可以给 `bootstrap`、`build`、`check` 添加 `--offline`。修改前端后先构建前端再构建 Rust；标准入口已经处理该顺序。不要用 `npm install` 或不带 `--locked` 的 Cargo 命令意外更新锁文件。

## 提交改动

1. 检查工作区、当前分支和 remote，保护其他人的修改；非微小变更先明确背景、范围、非目标与验收条件。
2. 在独立分支实现一个可解释的目标。自动化执行者默认使用 `codex/` 前缀，人工分支可采用 `feat/`、`fix/`、`docs/` 等主题前缀。
3. 遵循既有模块边界；不要为文件数量或行数拆分 crate。新增行为和 bug fix 需要有意义的测试。
4. 同步使用说明、契约或 ADR；验证报告记录当时事实，不复制当前进度。
5. 执行相关检查。提交前运行 `node scripts/dev.mjs binaries` 检查暂存内容；发布审计使用 `node scripts/dev.mjs binaries --all-branches`。
6. 使用 Conventional Commits 和 PR 模板，说明结果、验证与限制。不要把尚未运行的 CI、ignored 测试或真机检查写成通过。

维护者按范围、正确性、测试与可维护性评审；重大架构、安全和持久化变化先形成 ADR。没有已授权的远程写入时，自动化执行者只准备本地改动，不自动创建 Issue/PR、push、merge 或发布。

## 数据与产物

自动化测试使用临时 HOME/USERPROFILE、CODEX_HOME 和合成数据；不能写真实用户会话或工作台历史。新日志、截图与一次性构建放入受管任务目录；只有元数据有效且已完成的过期任务可自动清理。数据库、备份、Git bundle 和未分类旧目录不在自动清理范围内。

Git 可以追踪经审查的图片和文本 SVG；禁止追踪其他二进制、构建产物、归档包、数据库、secret 和私人 fixture，也不能用 Base64 包装绕过。图片许可与隐私要求同样适用。不要将完整本地日志、运行目录或原生会话直接上传到公开 Issue。

## 问题与安全报告

普通问题使用 GitHub Issue 模板，提供平台、版本、最小复现与脱敏结果。漏洞通过 [SECURITY.md](SECURITY.md) 的私下渠道报告。参与者遵守[社区行为规范](CODE_OF_CONDUCT.md)。

贡献的项目代码按 [MIT](LICENSE) 分发；第三方材料保留原许可与来源，参见[第三方声明](THIRD_PARTY_NOTICES.md)。
