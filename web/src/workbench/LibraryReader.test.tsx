import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { LibraryReader } from './LibraryReader';
import type { LibraryEntry, LibraryRecord } from './library';

const root = document.createElement('div');
const entry: LibraryEntry = { entryId: 'e', sourceId: 's', kind: 'native', sourceRevision: 'v1', title: '真实任务', projectId: 'p', projectPath: '/synthetic/app', recordedAt: null, nativeThreadId: 't', runId: null, coverage: { state: 'complete_for_source', reasons: [] }, capabilities: { read: true, resume: true, inspectCalls: false, manage: false }, relatedEntryIds: [] };
const route = { project: '', source: '', group: '', q: '', entry: 'e', revision: 'v1', match: '' };
const user: LibraryRecord = { kind: 'user_message', role: 'user', text: '修复发布入口', detailCursor: 'user', presentation: 'conversation' };
const context = (i: number): LibraryRecord => ({ kind: 'user_message', role: 'user', text: `# AGENTS.md instructions ${i}`, presentation: 'context', detailCursor: `context${i}` });
function mock(read: (index: number) => { records: LibraryRecord[]; nextCursor: string | null }) {
  const fetch = vi.fn(async (url: string) => {
    const u = new URL(url, location.href);
    return { ok: true, text: async () => JSON.stringify({ ...read(Number(u.searchParams.get('cursor') || 0)), entry, revision: 'v1', coverage: entry.coverage }) };
  }); vi.stubGlobal('fetch', fetch); return fetch;
}
async function flush() { for (let i = 0; i < 12; i++) await act(async () => { await new Promise(r => setTimeout(r, 2)); }); }
async function mount(match = '') {
  document.body.append(root);
  await act(async () => render(<LibraryReader route={{ ...route, match }} sources={[{ id: 's' }]} onBack={() => {}} onEntry={() => {}} />, root)); await flush();
}
async function expand() {
  await act(async () => { const fold = root.querySelector<HTMLDetailsElement>('.wb-library-record-fold')!; fold.open = true; fold.dispatchEvent(new Event('toggle')); }); await flush();
}
afterEach(() => { render(null, root); root.remove(); history.replaceState(null, '', '/'); vi.unstubAllGlobals(); });

it('seeks past a long confirmed injected prefix without parsing keywords or losing its entry', async () => {
  const fetch = mock(n => n < 3 ? { records: Array.from({ length: 16 }, (_, i) => context(n * 16 + i)), nextCursor: String(n + 1) } : { records: [user], nextCursor: null });
  await mount();
  expect(root.querySelector('[data-role=user]')?.textContent).toContain('修复发布入口');
  expect(root.textContent).not.toContain('# AGENTS.md instructions');
  expect(root.querySelector('.wb-library-record-fold summary')?.textContent).toContain('上下文');
  await expand(); expect(root.textContent).toContain('# AGENTS.md instructions');
  expect(root.querySelector('.wb-library-record-fold')?.textContent).toContain('会话上下文');
  expect(root.querySelector('.wb-library-record-fold')?.textContent).not.toContain('未识别记录');
  expect(fetch.mock.calls.every(([url]) => url.includes('sourceRevision=v1'))).toBe(true);
});
it('keeps ordinary requests about AGENTS visible, tools/unknown accessible and errors/gaps expanded', async () => {
  mock(() => ({ records: [{ ...user, text: '解释 AGENTS.md instructions 的含义' }, { kind: 'tool_call', text: 'echo synthetic', detailCursor: 'tool' }, { kind: 'future_variant', text: 'unknown retained', detailCursor: 'future' }, { kind: 'error', text: '失败证据', detailCursor: 'error' }, { kind: 'history_gap', text: '缺口证据', detailCursor: 'gap' }], nextCursor: null }));
  await mount(); expect(root.textContent).toContain('解释 AGENTS.md');
  expect(root.textContent).not.toContain('echo synthetic'); expect(root.textContent).toContain('未识别记录');
  expect(root.textContent).toContain('失败证据'); expect(root.textContent).toContain('缺口证据');
  await expand(); expect(root.textContent).toContain('unknown retained'); expect(root.textContent).toContain('echo synthetic');
});
it('expands the search window even for a context or unknown hit, without prefix seeking', async () => {
  const fetch = mock(() => ({ records: [context(0), { kind: 'future_variant', text: 'search target', detailCursor: 'future' }], nextCursor: '1' }));
  await mount('42'); expect(root.textContent).toContain('# AGENTS.md instructions'); expect(root.textContent).toContain('search target');
  expect(fetch.mock.calls[0][0]).toContain('record=42');
  expect(fetch.mock.calls.length).toBeLessThanOrEqual(2);
});
it('stops automatic prefix scanning at eight windows and leaves an explicit continuation', async () => {
  const fetch = mock(n => ({ records: Array.from({ length: 16 }, (_, i) => context(n * 16 + i)), nextCursor: String(n + 1) }));
  await mount(); expect(root.textContent).toContain('前 128 条记录尚未找到对话');
  // The ordinary virtual reader may prefetch one neighbor, never the whole history.
  expect(fetch.mock.calls.length).toBeLessThanOrEqual(9);
  expect(root.querySelectorAll('.wb-library-record-fold').length).toBeLessThanOrEqual(3);
});
it('does not expose expanded records or detail actions after source revocation', async () => {
  mock(() => ({ records: [user, context(0)], nextCursor: null })); await mount(); await expand();
  await act(async () => render(<LibraryReader route={route} sources={[]} onBack={() => {}} onEntry={() => {}} />, root));
  expect(root.textContent).toContain('此来源已被移除'); expect(root.querySelector('.wb-library-record')).toBeNull(); expect(root.querySelector('.wb-library-record-fold')).toBeNull();
});
it('keeps folds within the virtual window and reloads earlier raw records on scrollback', async () => {
  const fetch = mock(n => ({ records: [user, ...Array.from({ length: 15 }, (_, i) => ({ kind: 'tool_call', text: `window ${n} tool ${i}`, detailCursor: `${n}:${i}` }))], nextCursor: n < 20 ? String(n + 1) : null }));
  await mount(); const area = root.querySelector<HTMLElement>('.wb-library-body-scroll')!;
  for (let i = 1; i < 12; i++) await act(async () => { area.scrollTop = i * 700 + 1; area.dispatchEvent(new Event('scroll')); await new Promise(r => setTimeout(r, 0)); });
  await flush(); expect(root.querySelectorAll('.wb-library-record-fold').length).toBeLessThanOrEqual(3);
  await act(async () => { area.scrollTop = 0; area.dispatchEvent(new Event('scroll')); }); await flush(); await expand();
  expect(root.textContent).toContain('window 0 tool 0');
  expect(fetch.mock.calls.filter(([url]) => !url.includes('cursor=')).length).toBeGreaterThanOrEqual(2);
});
it('switches to raw records without restarting or losing the pinned source cursor', async () => {
  const fetch = mock(() => ({ records: [context(0), user], nextCursor: null })); await mount();
  const requests = fetch.mock.calls.length;
  await act(async () => { Array.from(root.querySelectorAll('button')).find(button => button.textContent === '全部记录')!.click(); });
  expect(root.textContent).toContain('# AGENTS.md instructions');
  await act(async () => { Array.from(root.querySelectorAll('button')).find(button => button.textContent === '对话')!.click(); });
  expect(root.querySelector('[data-role=user]')?.textContent).toContain('修复发布入口');
  expect(fetch.mock.calls.length).toBe(requests);
});
