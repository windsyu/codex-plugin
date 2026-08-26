# ADR 0008: Vite + Preact Web Viewer

## Context

现有 Web Viewer 是零构建的 vanilla JS/CSS，信息层级和可读性不足。需要 Markdown 渲染、代码高亮、项目分组、上下文折叠和更细的 item 类型展示。

## Decision

Web Viewer 重构为 Vite + Preact + TypeScript，保持无后端框架依赖。Markdown 使用 `marked`，安全清洗使用 `DOMPurify`，代码高亮使用 `highlight.js`。前端测试使用 Vitest + jsdom。构建产物输出到 `web/dist`，Rust 通过 `rust-embed` 嵌入并服务 `/` 与 `/assets/*`。Rust `build.rs` 在产物缺失时提示先执行 `npm run build`。CSP 继续使用 `script-src 'self'` 和 `style-src 'self'`，不引入 inline script/style。

## Alternatives

- 继续 vanilla JS：无构建链，但组件状态、富文本和测试都更脆弱。
- 使用 React：生态更大，但体积和启动成本高于当前单页 MVP 所需。
- 引入完整 SPA 框架和后端 SSR：超出 V1 本地 Viewer 的需要。

## Consequences

发布前需要先构建前端；Rust 二进制仍自包含，运行时不需要 `web/dist` 目录。新增 npm 依赖与 `node_modules` 工作流。所有用户内容在进入 DOM 前经过 DOMPurify，本地文件链接不会被渲染为可点击动作。

## Status

Accepted

## Date

2026-08-16
