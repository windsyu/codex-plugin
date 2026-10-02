# 公开发布与匿名安装验证（2026-10-02）

[验证索引](README.md) · [发布维护](../development/releasing.md) · [Issue #12](https://github.com/windsyu/codex-plugin/issues/12)

## 交付范围

用户明确选择公开现有 `windsyu/codex-plugin` 仓库（包括历史与 release），安装默认前缀使用 `~/.codex-view`，恢复 README 的产品和操作说明。快速脚本使用系统 curl 下载公开 HTTPS 资源，不需要 GitHub 账号、CLI、API 或 token。latest 只解析一次，严格限制同仓库的 `v0.x.y` tag，再从固定版本下载归档与 SHA256SUMS。

保留既有安装清单、shell 配置保护、原子替换与失败回滚。默认前缀检测到旧 `~/.local` 快速安装清单时，提示先显式卸载；不自动迁移个人文件。工作台数据目录仍为 `~/.codex-web`，原生 `CODEX_HOME` 和旧数据库保持原位。

沿用已有 `v0.3.0` 安装包；tag、二进制与归档校验和不改写。当前源码中的手动安装器和生成说明使用新默认前缀；已发布包的内置默认仍为 `.local`，当前指南通过显式 `--prefix` 使用新位置。

## 公开前审查

在 fetch 所有远端 heads/tags 后，独立审查覆盖 22 个 refs、50 个提交、1,256 个独立 blob。禁止二进制检查为 0；文本凭证格式与泛化候选复核均对应合成测试/脱敏材料；8 版 JSONL fixture 为固定合成数据。vt100 原许可与补丁说明、Codicons 许可与归属材料齐全。未发现需要历史重写的明确阻塞。

历史图片共 139 个，人工抽查 3 张并核对合成验证来源；本机 Vision OCR 对全量图片均未成功，因此不把它计为通过的完整图片文本检查。本审查不声称能够证明不存在所有敏感信息。文档中带上下文的维护者邮箱与历史本机路径仍作为原有证据保留。

## 安装回归

合成归档测试使用临时 HOME/USERPROFILE/CODEX_HOME，不访问真实用户数据。curl mock 覆盖匿名下载协议，gh mock 调用即失败；保留原有 receipt、卸载范围、PATH 字节还原、并发修改与失败回滚断言。

新增或更新的验证包括默认无参数安装、固定版本跳过 latest、固定 tag 同时下载归档和校验文件、无效/外仓库/非 HTTPS/预发布跳转拒绝、三个下载阶段 HTTP 失败零用户写入、旧 `.local` 显式卸载后重新安装、旧清单符号链接拒绝，以及 README 下载到实际临时文件再执行的完整入口。README 的 Bash/zsh 回归覆盖下载失败、临时文件失败、安装失败和成功后的 PATH 更新。未来包内安装器另验证默认前缀与显式重装。

独立评审的 8 个增量定点测试、Bash 语法检查与 diff 检查通过。首轮完整检查的维护工具测试为 79/80，一处旧手动安装默认路径断言仍指向 `.local`；修正后默认安装的 2 个专项通过，再重跑完整检查。最终 `node scripts/dev.mjs check` 通过：80 项维护工具、234 项前端、445 项 Rust，另 56 项 opt-in ignored 未运行；格式、Clippy、类型、Web/debug/release 构建全部通过。106 份文档检查无问题，13 个暂存文件与 1,256 个历史 blob 的二进制检查无禁止项。完整检查产物为 `target/artifacts/c1634ecc-527d-45c0-bb34-ce2be04f6a2e`。

完整本地检查、PR/main CI 和公开后的真实匿名下载结果在对应 Issue/PR 中分别登记，不以合成网络测试代替公开访问证明。公开后的实际验证须使用无凭证环境、仅系统工具 PATH、隔离 HOME，下载 main README 并执行其首个安装块，验证真实版本、来源、许可、两个 shell 的 PATH 和禁用网络工具后的离线卸载。

## 兼容与边界

不改变 Rust/Web 运行逻辑、API、schema 或用户数据格式，不扩大平台/provider 支持。发布范围继续为 macOS / Apple Silicon；使用者启动项目仍需已安装且配置受支持路由的官方 CLI。校验和检查传输完整性，不代替签名；本轮不新增 Apple 签名或 notarization。
