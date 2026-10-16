import { render } from 'preact';
import { afterEach, expect, it } from 'vitest';
import { ExchangePreview, exchangePreviewText } from './ExchangePreview';
const root = document.createElement('div');
afterEach(() => { render(null, root); root.remove(); });
it('shows readable prose without markdown syntax, embedded HTML or code process', () => {
  render(<ExchangePreview preview={{ role: 'assistant', text: '## 发布检查\n已补齐 **快速启动**。\n\n```rust\nfn internal() {}\n```\n<img src="https://example.invalid/blocked"><script>window.historyInjected=true</script>' }} />, root);
  expect(root.textContent).toBe('回复：发布检查 已补齐 快速启动。');
  expect(root.querySelector('img,script,code,h2')).toBeNull();
  expect(root.textContent).not.toContain('example.invalid');
});
it('retains a code-only exchange instead of inventing prose', () => {
  expect(exchangePreviewText('```rust\nlet result = 1;\n```')).toBe('let result = 1;');
  expect(exchangePreviewText('`cargo test`')).toBe('cargo test');
  expect(exchangePreviewText('请修复 `ProjectView` 的跳转')).toBe('请修复 ProjectView 的跳转');
  expect(exchangePreviewText('已调整布局。 ```rust fn internal')).toBe('已调整布局。');
});
it('does not replace a missing or media-only excerpt with another source', () => {
  render(<ExchangePreview preview={null} />, root); expect(root.textContent).toBe('');
  render(<ExchangePreview preview={{ role: 'user', text: '![](https://example.invalid/private)' }} />, root);
  expect(root.textContent).toBe(''); expect(root.querySelector('img')).toBeNull();
});
