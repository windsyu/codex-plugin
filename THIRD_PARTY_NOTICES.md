# 第三方材料与许可

项目原创代码采用根目录 [MIT 许可证](LICENSE)。第三方材料继续遵循各自许可；本清单不将这些材料重新许可为 MIT，也不代替 Cargo/npm 依赖的各自声明。

| 材料 | 来源、许可与修改说明 |
| --- | --- |
| vt100 0.16.2 本地补丁 | 来源、原始 checksum、补丁范围与移除条件见 [PATCH.md](vendor/vt100/PATCH.md)，原 [MIT 许可证](vendor/vt100/LICENSE) 必须保留 |
| Microsoft Codicons 子集 | 来源 commit、作者、20 个图形的提取方式见[图标说明](web/src/workbench/icons/README.md)，保留原 [CC BY 4.0 许可证](web/src/workbench/icons/LICENSE)及归属声明 |
| Rust 依赖 | 精确解析版本见 `Cargo.lock`；发布者需根据实际分发的依赖保留其要求的版权、许可和通知 |
| Web 依赖 | 精确解析版本见 `web/package-lock.json`；前端会嵌入 binary，发布前应核对随包分发要求 |
| 静态 Linux 运行库 | 指定测试中转的 musl 构建另附实际 Zig 工具链的 musl、LLVM libunwind、compiler runtime 及 Rust 标准库/内建函数原通知；这些运行库不由 Cargo/npm metadata 完整列出，不能只收集依赖索引 |

## 更新与发布

本机与中转二进制均通过 `codex-workbench-assets` 嵌入相同 Web 运行依赖与 Codicons；单独分发 `codex-relayd` 也必须附带这些许可和归属声明，不能因没有安装 Node.js 而省略。共享 crate 自身为项目 MIT 代码；发布许可收集必须包含它的 LICENSE。中转运行时不包含或依赖本机 CLI 模块。

引入第三方源码、图片、字体或其他资产时记录来源、版本/commit、许可和修改范围。图片可入 Git，但不因此免除许可和隐私审查。禁止未经验证复制官方 Codex 内部 crate 作为生产依赖。

分发包须包含项目 LICENSE、本声明及分发材料要求的第三方许可。`node scripts/dev.mjs package` 根据 host-resolved Cargo metadata 与已安装的 npm 运行依赖保留实际许可/通知文件，另包含上述补丁、归属与修改说明；结果索引为包内 `licenses/INDEX.json`。维护者仍需核对锁定依赖和实际分发内容；本清单不代替生成包的检查结果。

目标专用静态包装通过 `writeLicenses` 的显式 `runtimeLicenses` 输入保留运行库原文件，索引类别为 `toolchain-runtime`，并纳入全包校验和。Linux 测试包保守附带 musl/工具链相关原始文本以完整保留单文件通知；Rust 使用 `COPYRIGHT-library.html` 及原许可文本，不把编译器文档清单中的跨平台或可选组件认定为当前二进制依赖。此收集不会安装、链接或执行附带源码，也不代表新增平台发布支持。macOS 正式包装仍须按其实际 sysroot 独立核查，不能用 Linux 包的许可检查替代。
