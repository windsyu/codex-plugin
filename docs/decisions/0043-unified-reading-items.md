# ADR 0043：统一实时阅读项目与增量契约

- Status：Accepted（R2 增量，尚未代表整片通过）
- Date：2026-09-20

## Context

R1/R2 的增量实现分别用 items、tools、userMessages 数组保存浏览器阅读状态，SSE 也分为 item/tool/user 更新。正文第一条 patch 可以直接创建无角色项目，与详细设计要求的“首条有类型、后续原位更新”不一致。来源和终态补齐、未知类型、快照接续需要一份明确的公共契约。

这是本项目的 API 与呈现选择，不是官方 Codex 协议变化。当前工作台阅读 API 尚未发布，已有旧 `/v1` 只读契约和数据库不受影响。

## Decision

1. 快照使用 `schemaVersion=2`，唯一 `items: ViewItem[]` 包含 message、tool_call、notice。message 的 author 明确为 user 或 assistant；保留 contentKey、来源、稳定 itemKey、revision、orderIndex、截断与不完整性。
2. snapshot 与 item.replace 序列化相同类型。移除旧 tools/userMessages 数组及 tool.replace/tool.patch/user.replace，首次项目必须由 item.replace 创建。item.patch 为封闭 text/arguments 变体；正文必须指明 contentKey，版本或类型不匹配要求重新快照。
3. 模型来源与正文终态变化使用同键 replace。响应报告与请求模型分别保留；权威全文只结束对应文本，response completed 不证明工具执行或整个轮次结束。
4. 来源只输出实际已有信息：模型 TextKey/captureSeq、原生 thread/turn/item 与 opaque sourceRef/byteOffset、诊断位置。运行身份在 envelope 中提供。不补不存在的 connection/stream/request ID；工具结果保持独立 result.source。
5. 后端继续按来源类型维护既有有界聚合，不增加第二份长期内容缓存；snapshot 将每个来源项目转换为一次 typed item。前端只维护同一 items，渲染选择器即时派生模型/用户/工具列表。
6. 未来未知 kind/author 的合法 envelope 显示固定安全 notice，原 payload 丢弃，不猜角色。非法身份、内容键、增量类型或版本要求快照。来源 diagnostics 和原生事实数组继续用于诊断与核对，不是第二份聊天副本。
7. 保留内存 cursor、慢订阅者隔离、淘汰后快照和 epoch 边界；R0 诊断页、探针与正式工作台同时升级。run 返回 readingSchemaVersion=2。旧页面刷新加载新脚本，不增加旧草案适配层。

## Alternatives

- 仅增加第四个统一展示数组：造成三套原数据和新数组共同维护，没有消除接续与身份分歧。
- 只在浏览器按内容或 CSS 推断角色：无法可靠地区分用户提交、自动上下文、模型正文与工具调用。
- 把所有来源聚合重构成一个无类型字典：会扩大工具匹配与预算改动，对此次公共契约没有必要。
- 为尚未发布的草案保留双份事件：增加带宽、内存和重放歧义，消费者可随正式页面同步升级。

## Consequences

用户可见布局保持，更新行为明确。Chrome 注入丢失正文增量后会重新快照，原消息/工具 DOM 节点、展开状态和终端焦点保留；未知类型只显示安全提示。安装版 CLI 的实际命令、修改、迟到退出与轮询已回归，见[验证记录](../validation/native-cli-r2-view-items-2026-09-20.md)。

本次只替换内存阅读 API；无数据库 migration、原版 CLI 或全局配置修改，不重启现有用户调试进程。来源仍受当前已验证协议范围限制，复杂上下文关系与其他工具事实按 R2 余项继续验收。
