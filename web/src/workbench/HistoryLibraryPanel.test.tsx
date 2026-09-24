import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { HistoryLibraryPanel } from './HistoryLibraryPanel';
import type { LibraryEntry, LibraryRecord } from './library';
const root = document.createElement('div');
const entry: LibraryEntry = { entryId: 'e_a', title: '修复阅读界面', sourceId: 's', kind: 'native', sourceRevision: 'v1', projectId: 'p_a', projectPath: '/synthetic/app', recordedAt: '2026-09-22T02:00:00Z', nativeThreadId: 'thread-a', runId: null, coverage: { state: 'complete_for_source', reasons: [] }, capabilities: { read: true, inspectCalls: false, resume: true, manage: false }, relatedEntryIds: [] };
const sources = [{ id: 's', kind: 'native', state: 'ready', revision: 'source-v1', message: '已索引 1 条记录' }];
it.each(['indexing', 'read_only'])('rechecks an empty pending catalog while %s without a revision event', async state => {
  vi.useFakeTimers();
  try {
    history.replaceState(null, '', '/?project=p_a&group=main&q=needle');
    let published = false;
    const fetch = mockFetch(u => {
      if (!u.pathname.endsWith('/entries') && !u.pathname.endsWith('/projects')) return;
      const records = published ? u.pathname.endsWith('/entries') ? [entry] : [{ projectId: 'p_a', path: '/synthetic/app', entries: 1 }] : [];
      return { ok: true, text: async () => JSON.stringify(published ? page(records) : { ...page([]), revision: 'pending', coverage: { state: 'partial', reasons: ['indexing'] } }) };
    });
    document.body.append(root);
    await act(async () => render(<HistoryLibraryPanel sources={[{ ...sources[0], state, revision: null, indexedEntries: 17 }]} />, root));
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(root.textContent).toContain('已整理 17 条');
    published = true;
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(root.querySelector('.wb-library-entry')?.textContent).toContain(entry.title);
    const requests = fetch.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(fetch.mock.calls).toHaveLength(requests);
    expect(fetch.mock.calls.every(([url]) => !url.includes('/refresh'))).toBe(true);
    expect(location.search).toBe('?project=p_a&group=main&q=needle');
    expect(fetch.mock.calls.filter(([url]) => url.includes('/entries?')).every(([url]) => new URL(url, 'http://localhost').searchParams.get('projectId') === 'p_a')).toBe(true);
  } finally { render(null, root); vi.useRealTimers(); }
});
it('does not retry the empty background list while a history record is open', async () => {
  vi.useFakeTimers();
  try {
    history.replaceState(null, '', '/?entry=e_a&revision=v1');
    const fetch = mockFetch(u => u.pathname.endsWith('/entries') || u.pathname.endsWith('/projects') ? { ok: true, text: async () => JSON.stringify({ ...page([]), coverage: { state: 'partial', reasons: ['indexing'] } }) } : undefined);
    document.body.append(root);
    await act(async () => render(<HistoryLibraryPanel sources={sources} />, root));
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    const requests = fetch.mock.calls.filter(([url]) => url.includes('/entries?')).length;
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(fetch.mock.calls.filter(([url]) => url.includes('/entries?'))).toHaveLength(requests);
    expect(location.search).toBe('?entry=e_a&revision=v1');
  } finally { render(null, root); vi.useRealTimers(); }
});
it('stops empty catalog retries at definitive empty results and cancels them on unmount', async () => {
  vi.useFakeTimers();
  try {
    let pending = true;
    const fetch = mockFetch(u => u.pathname.endsWith('/entries') || u.pathname.endsWith('/projects') ? { ok: true, text: async () => JSON.stringify({ ...page([]), coverage: { state: pending ? 'partial' : 'complete_for_source', reasons: pending ? ['indexing'] : [] } }) } : undefined);
    document.body.append(root);
    await act(async () => render(<HistoryLibraryPanel sources={[{ ...sources[0], state: 'indexing', revision: null }]} />, root));
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    pending = false;
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(0); });
    expect(root.textContent).toContain('这里还没有历史记录');
    const requests = fetch.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(fetch.mock.calls).toHaveLength(requests);
    await act(async () => render(null, root));
    pending = true;
    await act(async () => render(<HistoryLibraryPanel sources={sources} />, root));
    await act(async () => render(null, root));
    const unmountedRequests = fetch.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(6000); });
    expect(fetch.mock.calls).toHaveLength(unmountedRequests);
  } finally { render(null, root); vi.useRealTimers(); }
});
const page = (records: unknown[], nextCursor: string | null = null) => ({ records, nextCursor, revision: 'v1', coverage: entry.coverage });
const messages: LibraryRecord[] = [{ kind: 'user_message', role: 'user', text: '你好 <script>unsafe</script>', detailCursor: 'user' }, { kind: 'agent_message', role: 'assistant', text: '**模型回复** <img src="https://example.invalid/private"> <script>unsafe</script>', detailCursor: 'model' }, { kind: 'tool_call', text: 'echo ok', detailCursor: 'tool' }];
function mockFetch(handler?: (url: URL) => unknown) {
  const fetch = vi.fn(async (url: string) => {
    const u = new URL(url, 'http://localhost'), custom = handler?.(u);
    if (custom !== undefined) return custom;
    let value: unknown = page([]);
    if (u.pathname.endsWith('/projects')) value = { ...page([{ projectId: 'p_a', path: '/synthetic/app', entries: 1 }]), unassignedRecords: 31 };
    else if (u.pathname.endsWith('/entries')) value = page([entry]);
    else if (u.pathname.endsWith('/details')) value = { ...page([{ raw: { payload: '<script>detail</script>', instructions: '已保存的提示词' } }]), entry };
    else if (u.pathname.includes('/entries/')) value = { ...page(messages), entry };
    return { ok: true, text: async () => JSON.stringify(value) };
  }); vi.stubGlobal('fetch', fetch); return fetch;
}
async function flush() { for (let i = 0; i < 4; i++) await act(async () => { await new Promise(r => setTimeout(r, 5)); }); }
async function mount() { document.body.append(root); await act(async () => { render(<HistoryLibraryPanel sources={sources} />, root); }); await flush(); }
async function click(text: string) { const button = Array.from(root.querySelectorAll('button')).find(b => b.textContent?.includes(text)); expect(button).toBeTruthy(); await act(async () => { button!.click(); }); await flush(); }
afterEach(() => { render(null, root); root.remove(); history.replaceState(null, '', '/'); vi.unstubAllGlobals(); });
it('opens read-only history and loads individual raw details only on demand, escaping unsafe content', async () => {
  const fetch = mockFetch(); await mount();
  expect(root.textContent).toContain('未归属项目'); await click('修复阅读界面');
  expect(root.querySelector('[data-role=user]')?.textContent).toContain('你好 <script>unsafe</script>');
  expect(root.querySelector('[data-role=assistant] strong')?.textContent).toBe('模型');
  expect(root.querySelector('[data-kind=tool_call]')?.textContent).toContain('不代表已执行成功');
  expect(root.querySelector('script,img,iframe')).toBeNull();
  expect(fetch.mock.calls.some(([url]) => url.includes('/details'))).toBe(false);
  await click('查看详情'); expect(root.textContent).toContain('已保存的提示词'); expect(root.querySelector('script')).toBeNull();
  await click('关闭详情'); expect(root.querySelector('.wb-library-detail')).toBeNull(); expect(document.activeElement?.textContent).toBe('查看详情');
  await click('返回会话列表'); expect(document.activeElement?.getAttribute('data-entry')).toBe('e_a');
  expect(fetch.mock.calls.every(([url]) => url.startsWith('/workbench/v1/library/'))).toBe(true);
  expect(root.textContent).not.toContain('开始新对话'); expect(root.querySelector('.xterm')).toBeNull();
});
it('binds filters, search hit location, and back/forward navigation to the URL', async () => {
  const fetch = mockFetch(u => u.pathname.endsWith('/entries') && u.searchParams.has('q') ? { ok: true, text: async () => JSON.stringify(page([{ ...entry, match: { record: 7, sourceRevision: 'v1', text: '命中的消息' } }])) } : undefined);
  await mount();
  await act(async () => { const input = root.querySelector('input')!; input.value = '命中'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  await flush(); await click('修复阅读界面');
  expect(location.search).toContain('match=7'); expect(fetch.mock.calls.some(([url]) => url.includes('record=7') && url.includes('sourceRevision=v1'))).toBe(true);
  await act(async () => { history.replaceState(null, '', '/?q=命中&project=unassigned'); dispatchEvent(new PopStateEvent('popstate')); });
  expect(root.querySelector('.wb-library-reader')).toBeNull(); expect(root.querySelector<HTMLInputElement>('input')?.value).toBe('命中');
  await flush(); expect(fetch.mock.calls.some(([url]) => url.includes('projectId=unassigned'))).toBe(true);
});
it('keeps the reading version when a source updates and does not append an expired page', async () => {
  mockFetch(u => u.pathname.endsWith('/e_a') ? u.searchParams.has('cursor') ? { ok: false, status: 409 } : { ok: true, text: async () => JSON.stringify({ ...page(messages, 'next'), entry }) } : undefined);
  await mount(); await click('修复阅读界面');
  expect(root.textContent).toContain('历史已更新'); expect(root.querySelectorAll('[data-role=user]')).toHaveLength(1);
  await act(async () => render(<HistoryLibraryPanel sources={[{ ...sources[0], revision: 'source-v2' }]} />, root));
  expect(root.textContent).toContain('当前阅读位置保持不变'); expect(location.search).toContain('revision=v1');
});
it('discards late list responses after changing a source', async () => {
  let resolve: (value: unknown) => void = () => {};
  const fetch = vi.fn(async (url: string) => {
    const u = new URL(url, 'http://localhost');
    if (u.pathname.endsWith('/entries') && !u.searchParams.has('sourceId')) return await new Promise(r => resolve = r);
    return { ok: true, text: async () => JSON.stringify(page(u.pathname.endsWith('/projects') ? [] : [{ ...entry, title: '新来源记录' }])) };
  }); vi.stubGlobal('fetch', fetch); await mount();
  await act(async () => { const select = root.querySelector('select')!; select.value = 's'; select.dispatchEvent(new Event('change', { bubbles: true })); });
  await act(async () => resolve({ ok: true, text: async () => JSON.stringify(page([{ ...entry, title: '迟到的旧记录' }])) }));
  await flush(); expect(root.textContent).toContain('新来源记录'); expect(root.textContent).not.toContain('迟到的旧记录');
});
it('hides previously loaded content when its source is revoked', async () => {
  mockFetch(); await mount(); await click('修复阅读界面');
  await act(async () => render(<HistoryLibraryPanel sources={[]} />, root));
  expect(root.textContent).toContain('此来源已被移除'); expect(root.querySelector('[data-role=user]')).toBeNull();
});
it('keeps a bounded number of message cards while scrolling a long history and revisits evicted windows', async () => {
  const fetch = mockFetch(u => {
    if (!u.pathname.endsWith('/e_a')) return;
    const n = Number(u.searchParams.get('cursor') || 0);
    return { ok: true, text: async () => JSON.stringify({ ...page(Array.from({ length: 16 }, (_, i) => ({ ...messages[0], text: `window ${n} row ${i}`, detailCursor: `${n}:${i}` })), n < 20 ? String(n + 1) : null), entry }) };
  }); await mount(); await click('修复阅读界面');
  const area = root.querySelector<HTMLElement>('.wb-library-body-scroll')!;
  for (let i = 1; i < 12; i++) await act(async () => { area.scrollTop = i * 700 + 1; area.dispatchEvent(new Event('scroll')); await new Promise(r => setTimeout(r, 0)); });
  expect(root.querySelectorAll('.wb-library-record').length).toBeLessThanOrEqual(48);
  await act(async () => { area.scrollTop = 0; area.dispatchEvent(new Event('scroll')); });
  await flush(); expect(fetch.mock.calls.filter(([u]) => u.includes('/e_a?') && !u.includes('cursor=')).length).toBeGreaterThanOrEqual(2);
});

it('restores a paginated session list after refreshing the same route', async () => {
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([{ ...entry, title: u.searchParams.has('cursor') ? '第二页会话' : '第一页会话' }], u.searchParams.has('cursor') ? null : 'page-two')) } : undefined);
  await mount(); await click('下一页'); expect(root.textContent).toContain('第二页会话');
  await act(async () => render(null, root)); await mount();
  expect(root.textContent).toContain('第二页会话'); expect(root.textContent).not.toContain('第一页会话');
});
it('explains a source reading budget instead of presenting an empty or complete conversation', async () => {
  mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: false, status: 503, json: async () => ({ error: 'source_read_budget' }) } : undefined);
  await mount(); await click('修复阅读界面');
  expect(root.querySelector('[role=alert]')?.textContent).toContain('超过读取限制');
  expect(root.textContent).not.toContain('已读到此来源的记录末尾');
});

it('keeps unassigned navigation outside project pagination with record counts', async () => {
  mockFetch(u => u.pathname.endsWith('/projects') ? { ok: true, text: async () => JSON.stringify({ ...page([{ projectId: 'p_a', path: '/synthetic/app', entries: 2 }], 'next-projects'), unassignedRecords: 31 }) } : undefined);
  await mount();
  const nav = root.querySelector('.wb-library-projects')!;
  expect(nav.textContent).toContain('全部会话');
  expect(nav.textContent).toContain('31 条记录');
  expect(nav.querySelector('.wb-library-project-list')?.textContent).not.toContain('未归属项目');
  await click('未归属项目');
  expect(location.search).toContain('project=unassigned');
});
it('shows an unassigned record without turning its cwd into a project and retains explicit resume', async () => {
  const unassigned = { ...entry, projectId: null, projectPath: null, recordedCwd: '/synthetic/Documents/Codex/2026-09-23/new-chat' };
  const resume = vi.fn();
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([unassigned])) } : u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: unassigned }) } : undefined);
  document.body.append(root);
  await act(async () => render(<HistoryLibraryPanel sources={sources} onResume={resume} />, root)); await flush();
  expect(root.querySelector('.wb-library-entry')?.textContent).toContain('未归属项目');
  expect(root.querySelector('.wb-library-entry')?.textContent).not.toContain('new-chat');
  await click('修复阅读界面');
  expect(root.querySelector('.wb-library-identity')?.textContent).toContain('工作目录：/synthetic/Documents/Codex/2026-09-23/new-chat');
  await click('继续此会话'); expect(resume).toHaveBeenCalledWith(unassigned);
});
it('clears only a confirmed obsolete project filter and preserves the open reading route', async () => {
  history.replaceState(null, '', '/?project=p_old&entry=e_a&revision=v1');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify({ ...page([]), projectExists: false }) } : undefined);
  await mount();
  expect(location.search).not.toContain('project=');
  expect(location.search).toContain('entry=e_a');
  expect(root.textContent).toContain('项目归类已更新');
  expect(root.querySelector('.wb-library-reader')).not.toBeNull();
});
it('retains project selection when the project is outside the current page or filters', async () => {
  history.replaceState(null, '', '/?project=p_elsewhere&group=agents');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify({ ...page([]), projectExists: true }) } : undefined);
  await mount(); expect(location.search).toContain('project=p_elsewhere');
  expect(root.textContent).not.toContain('项目归类已更新');
});

it('preserves a selected unassigned group with zero matches and applies filters to its total', async () => {
  history.replaceState(null, '', '/?project=unassigned&source=s&group=agents&q=hello');
  const fetch = mockFetch(u => u.pathname.endsWith('/projects') ? { ok: true, text: async () => JSON.stringify({ ...page([]), unassignedRecords: 0 }) } : undefined);
  await mount();
  const fixed = root.querySelector('.wb-library-projects button[aria-pressed=true]');
  expect(fixed?.textContent).toContain('未归属项目'); expect(fixed?.textContent).toContain('0 条记录');
  const projectRequest = new URL(fetch.mock.calls.find(([url]) => url.includes('/projects?'))![0], 'http://localhost');
  expect(projectRequest.searchParams.get('sourceId')).toBe('s');
  expect(projectRequest.searchParams.get('group')).toBe('agents');
  expect(projectRequest.searchParams.get('q')).toBe('hello');
  expect(projectRequest.searchParams.has('projectId')).toBe(false);
  expect(root.textContent).not.toContain('开始新对话');
});
it('does not invent a stale project when validity is unknown during indexing', async () => {
  history.replaceState(null, '', '/?project=p_pending');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([])) } : undefined);
  await mount(); expect(location.search).toContain('project=p_pending');
  expect(root.querySelector('.wb-library-list-heading')?.textContent).toBe('所选项目');
});

it('explains an unavailable parent without offering a broken jump', async () => {
  mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: { ...entry, parentThreadId: 'missing-parent', parentEntryId: null } }) } : undefined);
  await mount(); await click('修复阅读界面');
  expect(root.querySelector('.wb-library-identity')?.textContent).toContain('父会话当前不可用');
  expect(Array.from(root.querySelectorAll('button')).some(b => b.textContent === '查看父会话')).toBe(false);
});

it('revokes parent and related navigation when the current source is removed', async () => {
  mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: { ...entry, parentThreadId: 'parent', parentEntryId: 'e_parent', relatedEntryIds: ['e_related'] } }) } : undefined);
  await mount(); await click('修复阅读界面');
  await act(async () => render(<HistoryLibraryPanel sources={[]} />, root));
  for (const label of ['查看父会话', '查看关联来源的记录']) {
    const button = Array.from(root.querySelectorAll('button')).find(b => b.textContent === label);
    expect(button?.disabled).toBe(true);
  }
  expect(location.search).toContain('entry=e_a');
});
