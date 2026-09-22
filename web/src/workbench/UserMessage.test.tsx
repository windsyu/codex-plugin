import { render } from 'preact';
import { afterEach, describe, expect, it } from 'vitest';
import { UserMessage } from './UserMessage';
import type { UserRecord } from './reading';

const root = document.createElement('div');
const user: UserRecord = { key: { codexThreadId: 'thread', codexTurnId: 'turn', nativeItemId: 'item' }, role: 'user', text: '中文\n<script>alert(1)</script>\n**原样代码**', revision: 1, truncated: false, omitted: false, source: { sourceRef: 'source', byteOffset: 12, ordinal: 4 } };
afterEach(() => render(null, root));
describe('native user bubble', () => {
  it('labels the author and preserves literal multiline content without executing or rendering Markdown', () => {
    render(<UserMessage user={user} />, root);
    expect(root.querySelector('[data-role="user"]')).not.toBeNull();
    expect(root.querySelector('.wb-message-meta strong')?.textContent).toBe('用户');
    expect(root.querySelector('.wb-user-body')?.textContent).toBe(user.text);
    expect(root.querySelector('script')).toBeNull();
    expect(root.querySelector('.wb-user-body strong')).toBeNull();
  });
  it('shows omissions and truncation without presenting an empty submission as complete', () => {
    render(<UserMessage user={{ ...user, text: '', truncated: true, omitted: true }} />, root);
    expect(root.textContent).toContain('没有可展示的文本');
    expect(root.textContent).toContain('文本预览已截断');
    expect(root.textContent).toContain('尚未接入的非文本内容');
  });
  it('does not claim a complete interleaving for multiple user events in the same turn', () => {
    render(<UserMessage user={user} orderUnconfirmed />, root);
    expect(root.textContent).toContain('与模型片段的先后尚未确认');
  });
});
