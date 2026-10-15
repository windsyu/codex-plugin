import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { HistoryLibraryPanel } from './HistoryLibraryPanel';
import type { LibraryEntry, LibraryRecord } from './library';
const root = document.createElement('div');
const entry: LibraryEntry = { entryId: 'e_a', title: '修复阅读界面', sourceId: 's', kind: 'native', sourceRevision: 'v1', projectId: 'p_a', projectPath: '/synthetic/app', recordedAt: '2026-09-22T02:00:00Z', nativeThreadId: 'thread-a', runId: null, coverage: { state: 'complete_for_source', reasons: [] }, capabilities: { read: true, inspectCalls: false, resume: true, manage: false }, relatedEntryIds: [] };
const sources = [{ id: 's', kind: 'native', state: 'ready', revision: 'source-v1', message: '已索引 1 条记录' }];
// Keep the legacy entries/A contract suite explicit; default groups have dedicated tests below.
beforeEach(() => { if (!location.search) history.replaceState(null, '', '/?view=records'); });
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
  expect(root.textContent).toContain('未归类会话'); await click('修复阅读界面');
  expect(root.querySelector('[data-role=user]')?.textContent).toContain('你好 <script>unsafe</script>');
  expect(root.querySelector('[data-role=assistant] strong')?.textContent).toBe('模型');
  await act(async () => { const fold = root.querySelector<HTMLDetailsElement>('.wb-library-record-fold')!; fold.open = true; fold.dispatchEvent(new Event('toggle')); });
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
  await act(async () => { const input = root.querySelector<HTMLInputElement>('[aria-label="搜索会话和消息"]')!; input.value = '命中'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { root.querySelector('.wb-library-filters form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  await flush(); await click('修复阅读界面');
  expect(location.search).toContain('match=7'); expect(fetch.mock.calls.some(([url]) => url.includes('record=7') && url.includes('sourceRevision=v1'))).toBe(true);
  await act(async () => { history.replaceState(null, '', '/?q=命中&project=unassigned'); dispatchEvent(new PopStateEvent('popstate')); });
  expect(root.querySelector('.wb-library-reader')).toBeNull(); expect(root.querySelector<HTMLInputElement>('[aria-label="搜索会话和消息"]')?.value).toBe('命中');
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
  expect(nav.textContent).toContain('全部项目');
  expect(nav.textContent).toContain('31 条记录');
  expect(nav.querySelector('.wb-library-project-list')?.textContent).not.toContain('未归类会话');
  await click('未归类会话');
  expect(location.search).toContain('project=unassigned');
});
it('shows an unassigned record without turning its cwd into a project and retains explicit resume', async () => {
  const unassigned = { ...entry, projectId: null, projectPath: null, recordedCwd: '/synthetic/Documents/Codex/2026-09-23/new-chat' };
  const resume = vi.fn();
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([unassigned])) } : u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: unassigned }) } : undefined);
  document.body.append(root);
  await act(async () => render(<HistoryLibraryPanel sources={sources} onResume={resume} />, root)); await flush();
  expect(root.querySelector('.wb-library-entry')?.textContent).toContain('未归类会话');
  expect(root.querySelector('.wb-library-entry')?.textContent).not.toContain('new-chat');
  await click('修复阅读界面');
  expect(root.querySelector('.wb-library-identity')?.textContent).toContain('工作目录：/synthetic/Documents/Codex/2026-09-23/new-chat');
  await click('继续此会话'); expect(resume).toHaveBeenCalledWith(unassigned);
});
it('clears only a confirmed obsolete project filter and preserves the open reading route', async () => {
  history.replaceState(null, '', '/?view=records&project=p_old&entry=e_a&revision=v1');
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
  expect(fixed?.textContent).toContain('未归类会话'); expect(fixed?.textContent).toContain('0 条记录');
  const projectRequest = new URL(fetch.mock.calls.find(([url]) => url.includes('/projects?'))![0], 'http://localhost');
  expect(projectRequest.searchParams.has('sourceId')).toBe(false);
  expect(projectRequest.searchParams.has('group')).toBe(false);
  expect(projectRequest.searchParams.has('q')).toBe(false);
  expect(projectRequest.searchParams.has('projectId')).toBe(false);
  expect(root.textContent).not.toContain('开始新对话');
});
it('does not invent a stale project when validity is unknown during indexing', async () => {
  history.replaceState(null, '', '/?project=p_pending');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([])) } : undefined);
  await mount(); expect(location.search).toContain('project=p_pending');
  expect(root.querySelector('h1')?.textContent).toBe('所选项目');
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
    expect(button).toBeUndefined();
  }
  expect(location.search).toContain('entry=e_a');
});
it('keeps project navigation independent of message, source and group filters', async () => {
  history.replaceState(null, '', '/?projectQ=app&q=needle&source=s&group=agents');
  const fetch = mockFetch(); await mount();
  const projectRequest = new URL(fetch.mock.calls.find(([url]) => url.includes('/projects?'))![0], 'http://localhost');
  expect(projectRequest.searchParams.get('projectQ')).toBe('app');
  for (const field of ['q', 'sourceId', 'group']) expect(projectRequest.searchParams.has(field)).toBe(false);
  expect(root.querySelector<HTMLInputElement>('[aria-label="搜索项目"]')?.value).toBe('app');
});
it('retains selected project identity without matches or a matching project page', async () => {
  history.replaceState(null, '', '/?view=records&project=p_elsewhere&q=missing');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify({ ...page([]), selectedProject: { projectId: 'p_elsewhere', path: '/synthetic/elsewhere', status: 'available' }, totalRecords: 0 }) } : undefined);
  await mount();
  expect(root.querySelector('h1')?.textContent).toContain('elsewhere');
  const path = root.querySelector('.wb-library-selected-path');
  expect(path?.querySelector('summary')).not.toBeNull();
  expect(path?.textContent).toContain('/synthetic/elsewhere');
  expect(root.textContent).toContain('当前筛选 · 0 条来源记录');
});
it('makes the selected project the main heading while preserving its full-path identity', async () => {
  history.replaceState(null, '', '/?project=p_elsewhere&q=missing');
  mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([]), selectedProject: { projectId: 'p_elsewhere', path: '/synthetic/elsewhere', status: 'available' }, counts: { primary: { groups: 0, records: 0 }, auxiliary: { groups: 0, records: 0 }, runs: { groups: 0, records: 0 } } }) } : undefined);
  await mount();
  expect(root.querySelector('h1')?.textContent).toBe('elsewhere');
  expect(root.querySelector('.wb-library-selected-path')?.textContent).toContain('/synthetic/elsewhere');
  expect(root.querySelector('.wb-library-section-heading')?.textContent).not.toContain('展示组');
  expect(root.querySelector('.wb-library-section-heading')?.textContent).not.toContain('来源记录');
  expect(root.querySelector('.wb-library-count-details')?.textContent).toContain('0 个展示组');
});
it('shows the entry preview in all-records mode without repeating cwd or source tags in the primary row', async () => {
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([{ ...entry, preview: { text: '已找到渲染错位原因，等待下一步确认。', role: 'assistant' } }])) } : undefined);
  await mount();
  const row = root.querySelector('.wb-library-entry')!;
  expect(row.textContent).toContain('已找到渲染错位原因');
  expect(row.textContent).not.toContain('/synthetic/app');
  expect(row.textContent).not.toContain('原生会话');
  expect(root.querySelector<HTMLDetailsElement>('.wb-library-entry-info')?.open).toBe(false);
});
it('does not infer selected project from an unrelated entry and disables missing directory launch', async () => {
  history.replaceState(null, '', '/?view=records&project=p_missing');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify({ ...page([entry]), selectedProject: { projectId: 'p_missing', path: '/synthetic/missing', status: 'missing' } }) } : undefined);
  document.body.append(root); await act(async () => render(<HistoryLibraryPanel sources={sources} onProjectLaunch={vi.fn()} />, root)); await flush();
  expect(root.querySelector('h1')?.textContent).toContain('missing');
  expect(Array.from(root.querySelectorAll('button')).find(b => b.textContent === '开始新对话')?.disabled).toBe(true);
});
it('explains an unassigned reason without exposing it as a project', async () => {
  const unassigned = { ...entry, projectId: null, projectPath: null, unassignedReason: 'desktop_generated' };
  mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: unassigned }) } : undefined);
  await mount(); await click('修复阅读界面');
  expect(root.textContent).toContain('未归类原因：应用生成的工作目录');
});
it('keeps project search and its popstate value separate while retaining an open reader', async () => {
  const fetch = mockFetch(); await mount(); await click('修复阅读界面');
  const pathDetail = root.querySelector<HTMLDetailsElement>('.wb-library-project-path')!;
  expect(pathDetail.querySelector('summary')).not.toBeNull(); pathDetail.open = true;
  expect(pathDetail.textContent).toContain('/synthetic/app');
  await act(async () => { const input = root.querySelector<HTMLInputElement>('[aria-label="搜索项目"]')!; input.value = 'new'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await click('查找项目');
  expect(location.search).toContain('projectQ=new'); expect(location.search).toContain('entry=e_a');
  expect(root.querySelector('[data-role=user]')).not.toBeNull();
  expect(fetch.mock.calls.some(([url]) => url.includes('/projects?') && url.includes('projectQ=new'))).toBe(true);
  await act(async () => { history.replaceState(null, '', '/?projectQ=back&entry=e_a&revision=v1'); dispatchEvent(new PopStateEvent('popstate')); });
  await flush(); expect(root.querySelector<HTMLInputElement>('[aria-label="搜索项目"]')?.value).toBe('back');
});
it('does not borrow a record path for a selected project with no reliable summary', async () => {
  history.replaceState(null, '', '/?project=p_other');
  mockFetch(u => u.pathname.endsWith('/entries') ? { ok: true, text: async () => JSON.stringify(page([entry])) } : undefined);
  await mount();
  expect(root.querySelector('h1')?.textContent).toBe('所选项目');
  expect(root.querySelector('.wb-library-selected-path')).toBeNull();
});
it('distinguishes scheduled scans, a new directory and scan failure', async () => {
  let failed = false;
  mockFetch(u => u.pathname.endsWith('/refresh') ? { ok: !failed, json: async () => ({ scheduled: true, refreshId: 1 }) } : undefined);
  await mount(); await click('更新历史');
  expect(root.querySelector('[data-update-state="scanning"]')?.textContent).toContain('已安排后台扫描');
  expect(Array.from(root.querySelectorAll('button')).some(b => b.textContent === '载入新结果')).toBe(false);
  await act(async () => render(<HistoryLibraryPanel sources={[{ ...sources[0], revision: 'new-directory' }]} />, root));
  expect(root.querySelector('[data-update-state="available"]')?.textContent).toContain('新目录可读取');
  failed = true; await click('更新历史');
  expect(root.querySelector('[data-update-state="failed"]')?.textContent).toContain('扫描失败');
});
it('finishes an unchanged scan without blocking future scans', async () => {
  mockFetch(u => u.pathname.endsWith('/refresh') ? { ok: true, json: async () => ({ scheduled: true, refreshId: 1 }) } : u.pathname.endsWith('/sources') ? { ok: true, text: async () => JSON.stringify({ sources, refresh: { requested: 1, completed: 1, failed: false } }) } : undefined);
  await mount(); await click('更新历史');
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 1100)); }); await flush();
  expect(root.querySelector('[data-update-state="info"]')?.textContent).toContain('后台扫描已结束');
  expect(Array.from(root.querySelectorAll('button')).find(b => b.textContent?.includes('更新历史'))?.disabled).toBe(false);
});
it('never enables missing project launch from a fallback page while filters reload or fail', async () => {
  history.replaceState(null, '', '/?project=p_a');
  let changed = false;
  let finish: (value: unknown) => void = () => {};
  const fetch = vi.fn(async (url: string) => {
    const u = new URL(url, 'http://localhost');
    if (u.pathname.endsWith('/projects')) return { ok: true, text: async () => JSON.stringify(page([{ projectId: 'p_a', path: '/synthetic/app', entries: 1 }])) };
    if (changed) return new Promise(resolve => { finish = resolve; });
    return { ok: true, text: async () => JSON.stringify({ ...page([entry]), selectedProject: { projectId: 'p_a', path: '/synthetic/app', status: 'missing' } }) };
  }); vi.stubGlobal('fetch', fetch);
  document.body.append(root); await act(async () => render(<HistoryLibraryPanel sources={sources} onProjectLaunch={vi.fn()} />, root)); await flush();
  const start = () => Array.from(root.querySelectorAll('button')).find(b => b.textContent === '开始新对话');
  expect(start()?.disabled).toBe(true);
  changed = true;
  await act(async () => { const select = root.querySelector<HTMLSelectElement>('[aria-label="历史来源筛选"]')!; select.value = 's'; select.dispatchEvent(new Event('change', { bubbles: true })); }); await flush();
  expect(start()?.disabled).toBe(true);
  await act(async () => finish({ ok: false, status: 503 })); await flush();
  expect(start()?.disabled).toBe(true);
});
it.each([false, true])('observes ready-to-ready scan completion without an indexing sample, failed=%s', async failed => {
  const fetch = mockFetch(u => u.pathname.endsWith('/refresh') ? { ok: true, json: async () => ({ scheduled: true, refreshId: 7 }) } : u.pathname.endsWith('/sources') ? { ok: true, text: async () => JSON.stringify({ sources, refresh: { requested: 8, completed: 8, failed } }) } : undefined);
  await mount(); await click('更新历史');
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 1100)); }); await flush();
  expect(root.querySelector(`[data-update-state="${failed ? 'failed' : 'info'}"]`)?.textContent).toContain(failed ? '扫描失败' : '后台扫描已结束');
  expect(fetch.mock.calls.some(([url]) => url.endsWith('/sources'))).toBe(true);
});
it('ends scanning when refresh status cannot be read', async () => {
  mockFetch(u => u.pathname.endsWith('/refresh') ? { ok: true, json: async () => ({ scheduled: true, refreshId: 1 }) } : u.pathname.endsWith('/sources') ? { ok: false } : undefined);
  await mount(); await click('更新历史');
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 1100)); }); await flush();
  expect(root.querySelector('[data-update-state="failed"]')?.textContent).toContain('无法确认后台扫描结果');
});
it('aborts a pending status read and never overwrites a newly available directory', async () => {
  let finish: (value: unknown) => void = () => {};
  let signal: AbortSignal | undefined;
  const fetch = mockFetch(u => u.pathname.endsWith('/refresh') ? { ok: true, json: async () => ({ scheduled: true, refreshId: 1 }) } : undefined);
  const original = fetch.getMockImplementation()!;
  fetch.mockImplementation(async (url: string, options?: RequestInit) => {
    if (url.endsWith('/sources')) { signal = options?.signal || undefined; return await new Promise<Awaited<ReturnType<typeof original>>>(resolve => { finish = value => resolve(value as Awaited<ReturnType<typeof original>>); }); }
    return original(url);
  });
  await mount(); await click('更新历史');
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 1100)); });
  await act(async () => render(<HistoryLibraryPanel sources={[{ ...sources[0], revision: 'published' }]} />, root));
  expect(signal?.aborted).toBe(true);
  await act(async () => finish({ ok: true, text: async () => JSON.stringify({ sources, refresh: { requested: 1, completed: 1, failed: false } }) })); await flush();
  expect(root.querySelector('[data-update-state="available"]')?.textContent).toContain('新目录可读取');
  await click('更新历史');
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 1100)); });
  await act(async () => render(null, root));
  expect(signal?.aborted).toBe(true);
});
const groupedMember = { entryId: 'e_a', sourceId: 's', kind: 'native', sourceRevision: 'v1', title: '主要会话', coverage: entry.coverage, capabilities: entry.capabilities };
const grouped = { groupId: 'g_a', section: 'primary', conversationKind: 'unknown', preferredEntryId: 'e_a', members: [groupedMember], projectId: 'p_a', projectPath: '/synthetic/app', latestRecordedAt: entry.recordedAt };
const groupCounts = { primary: { groups: 1, records: 1 }, auxiliary: { groups: 2, records: 3 }, runs: { groups: 1, records: 1 } };
it('explains the global grouping budget and keeps the source-record view available', async () => {
  history.replaceState(null, '', '/');
  mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: false, status: 413, json: async () => ({ error: { code: 'query_budget_exceeded' } }) } : undefined);
  await mount();
  expect(root.querySelector('[role="alert"]')?.textContent).toContain('整理视图达到目录查询上限，可切换“全部来源记录”');
  await act(async () => { const select = root.querySelector<HTMLSelectElement>('[aria-label="历史展示方式"]')!; select.value = 'records'; select.dispatchEvent(new Event('change', { bubbles: true })); }); await flush();
  expect(location.search).toContain('view=records');
  expect(root.querySelector('[data-entry="e_a"]')).not.toBeNull();
});
it('defaults to primary groups and only loads auxiliary records when expanded', async () => {
  history.replaceState(null, '', '/');
  const fetch = mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([{ ...grouped, section: u.searchParams.get('section') || 'primary' }]), counts: groupCounts, organizationRevision: 'absent' }) } : undefined);
  await mount();
  expect(root.textContent).toContain('类型未确认');
  expect(root.querySelector('.wb-library-count-details')?.textContent).toContain('辅助会话 · 2 个展示组');
  expect(root.querySelector('.wb-library-section-toggle')?.textContent).toBe('辅助会话');
  expect(fetch.mock.calls.some(([url]) => url.includes('/session-groups?') && url.includes('section=primary'))).toBe(true);
  expect(fetch.mock.calls.some(([url]) => url.includes('section=auxiliary') || url.includes('section=runs'))).toBe(false);
  await click('辅助会话');
  expect(location.search).toContain('section=auxiliary');
  expect(fetch.mock.calls.some(([url]) => url.includes('section=auxiliary'))).toBe(true);
});
it('opens the preferred observer search member with its own revision and match', async () => {
  history.replaceState(null, '', '/?q=needle');
  const observer = { ...groupedMember, entryId: 'e_observer', kind: 'observer', sourceRevision: 'observer-v', title: '旧来源命中', match: { record: 17, text: 'needle', sourceRevision: 'observer-v' } };
  const fetch = mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([{ ...grouped, preferredEntryId: observer.entryId, members: [observer] }]), counts: groupCounts, organizationRevision: 'absent' }) } : undefined);
  await mount(); await click('旧来源命中');
  expect(location.search).toContain('entry=e_observer'); expect(location.search).toContain('revision=observer-v'); expect(location.search).toContain('match=17');
  expect(fetch.mock.calls.some(([url]) => url.includes('/e_observer?') && url.includes('record=17') && url.includes('sourceRevision=observer-v'))).toBe(true);
});
it('preserves legacy explicit group URLs and switches back to grouped display without losing filters', async () => {
  history.replaceState(null, '', '/?group=agents&q=needle&projectQ=app');
  const fetch = mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([grouped]), counts: groupCounts, organizationRevision: 'absent' }) } : undefined);
  await mount();
  expect(fetch.mock.calls.some(([url]) => url.includes('/entries?') && url.includes('group=agents'))).toBe(true);
  expect(fetch.mock.calls.some(([url]) => url.includes('/session-groups'))).toBe(false);
  await act(async () => { const select = root.querySelector<HTMLSelectElement>('[aria-label="历史展示方式"]')!; select.value = 'groups'; select.dispatchEvent(new Event('change', { bubbles: true })); }); await flush();
  expect(location.search).toContain('view=groups'); expect(location.search).toContain('q=needle'); expect(location.search).toContain('projectQ=app'); expect(location.search).not.toContain('group=');
  expect(fetch.mock.calls.some(([url]) => url.includes('/session-groups?') && url.includes('section=primary'))).toBe(true);
});
it('keeps grouped pagination and section in URL across list restoration', async () => {
  history.replaceState(null, '', '/?view=groups&section=runs');
  const fetch = mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([{ ...grouped, groupId: u.searchParams.get('section') === 'runs' ? 'g_run' : 'g_a', section: u.searchParams.get('section') || 'primary', members: [{ ...groupedMember, title: u.searchParams.has('cursor') ? '第二页组' : '第一页组' }] }], u.searchParams.get('section') === 'primary' && !u.searchParams.has('cursor') ? 'next-group' : null), counts: groupCounts, organizationRevision: 'absent' }) } : undefined);
  await mount(); await click('下一页'); expect(root.textContent).toContain('第二页组');
  await act(async () => render(null, root)); await mount();
  expect(location.search).toContain('section=runs'); expect(root.textContent).toContain('第二页组');
  expect(fetch.mock.calls.filter(([url]) => url.includes('section=auxiliary'))).toHaveLength(0);
  await act(async () => { history.replaceState(null, '', '/?view=groups&section=auxiliary'); dispatchEvent(new PopStateEvent('popstate')); }); await flush();
  expect(root.querySelector('[aria-label="辅助会话"]')).not.toBeNull();
});
it('puts reliable parent navigation above folded record details and explains conflicting parents', async () => {
  mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: { ...entry, isSubagent: true, parentEntryId: 'e_parent', parentThreadId: 'parent' } }) } : undefined);
  await mount(); await click('修复阅读界面');
  expect(root.querySelector('.wb-library-parent')?.textContent).toContain('查看父会话');
  expect(root.querySelector('.wb-library-identity button')?.textContent).not.toBe('查看父会话');
});
it('never joins auxiliary rows from a newer directory to an old primary list', async () => {
  history.replaceState(null, '', '/');
  mockFetch(u => u.pathname.endsWith('/session-groups') ? { ok: true, text: async () => JSON.stringify({ ...page([{ ...grouped, section: u.searchParams.get('section') || 'primary', members: [{ ...groupedMember, title: u.searchParams.get('section') === 'auxiliary' ? '较新辅助内容' : '原主要会话' }] }]), revision: u.searchParams.get('section') === 'auxiliary' ? 'new-directory' : 'v1', counts: groupCounts, organizationRevision: 'absent' }) } : undefined);
  await mount(); await click('辅助会话');
  expect(root.textContent).not.toContain('较新辅助内容'); expect(root.textContent).toContain('载入新结果后查看');
});
it('offers explicit owner-only organization without replacing the selected source reader', async () => {
  const fetch = mockFetch();
  document.body.append(root);
  await act(async () => render(<HistoryLibraryPanel sources={sources} instanceId="application" />, root)); await flush(); await click('修复阅读界面');
  expect(Array.from(root.querySelectorAll('button')).some(button => button.textContent === '调整项目归属')).toBe(true);
  expect(location.search).toContain('entry=e_a'); expect(location.search).toContain('revision=v1');
  expect(fetch.mock.calls.filter(([url]) => url.includes('/organization'))).toHaveLength(0);
});
it('refreshes organization lists after saving without refetching body, losing its anchor or changing the URL', async () => {
  history.replaceState(null, '', '/?view=records&project=p_a');
  const automatic = { automatic: { projectId: 'p_a', projectPath: '/synthetic/app', basis: 'cwd_inferred' }, effective: { projectId: 'p_a', projectPath: '/synthetic/app', basis: 'automatic' }, overrideState: 'none', conversationKind: 'ordinary', kindBasis: 'known', unassignedReason: null };
  let saved = false;
  const fetch = vi.fn(async (url: string, request: RequestInit) => {
    const u = new URL(url, 'http://localhost');
    if (u.pathname.endsWith('/organization')) { if (request.method === 'PUT') saved = true; return { ok: true, text: async () => JSON.stringify({ organization: saved ? { ...automatic, effective: { projectId: null, projectPath: null, basis: 'user_unassigned' }, overrideState: 'applied', unassignedReason: 'user_unassigned' } : automatic, organizationRevision: saved ? 'store:1' : 'absent', revision: saved ? 'new' : 'v1', canAdjust: true }) }; }
    if (u.pathname.endsWith('/projects')) return { ok: true, text: async () => JSON.stringify({ ...page([]), revision: saved ? 'new' : 'v1', unassignedRecords: saved ? 1 : 0 }) };
    if (u.pathname.endsWith('/entries')) return { ok: true, text: async () => JSON.stringify({ ...page([entry]), projectExists: !saved }) };
    return { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: { ...entry, sourceIdentity: 'identity', recordedCwd: '/original/execution' } }) };
  }); vi.stubGlobal('fetch', fetch);
  document.body.append(root); await act(async () => render(<HistoryLibraryPanel sources={sources} instanceId="application" />, root)); await flush(); await click('修复阅读界面');
  const body = root.querySelector<HTMLElement>('.wb-library-body-scroll')!; body.scrollTop = 45;
  await act(async () => { body.dispatchEvent(new Event('scroll')); });
  const url = location.search;
  const bodyReads = fetch.mock.calls.filter(([url]) => new URL(url, 'http://localhost').pathname.endsWith('/e_a')).length;
  const lists = fetch.mock.calls.filter(([url]) => new URL(url, 'http://localhost').pathname.endsWith('/projects')).length;
  await click('调整项目归属');
  await act(async () => { const select = root.querySelector<HTMLSelectElement>('[aria-label="调整方式"]')!; select.value = 'unassigned'; select.dispatchEvent(new Event('change', { bubbles: true })); }); await flush();
  await act(async () => { const confirm = root.querySelector<HTMLInputElement>('input[type=checkbox]')!; confirm.checked = true; confirm.dispatchEvent(new Event('change', { bubbles: true })); }); await flush();
  await click('保存归属'); await click('关闭');
  expect(location.search).toBe(url); expect(body.scrollTop).toBe(45); expect(root.querySelector('.wb-library-body-scroll')).toBe(body);
  expect(fetch.mock.calls.filter(([url]) => new URL(url, 'http://localhost').pathname.endsWith('/e_a'))).toHaveLength(bodyReads);
  expect(fetch.mock.calls.filter(([url]) => new URL(url, 'http://localhost').pathname.endsWith('/projects')).length).toBeGreaterThan(lists);
  expect(document.activeElement?.textContent).toBe('调整项目归属');
  expect(root.querySelector('.wb-library-identity')?.textContent).toContain('用户设为未归类');
  await click('返回会话列表');
  expect(location.search).not.toContain('project=');
});
it('reads original body and keeps explicit native resume when organization storage is unavailable', async () => {
  const organization = { automatic: { projectId: 'p_a', projectPath: '/synthetic/app', basis: 'cwd_inferred' }, effective: { projectId: 'p_a', projectPath: '/synthetic/app', basis: 'automatic' }, overrideState: 'unavailable', conversationKind: 'ordinary', kindBasis: 'known', unassignedReason: null };
  const fetch = mockFetch(u => u.pathname.endsWith('/e_a') ? { ok: true, text: async () => JSON.stringify({ ...page(messages), entry: { ...entry, organization } }) } : undefined);
  const resume = vi.fn(); document.body.append(root); await act(async () => render(<HistoryLibraryPanel sources={sources} instanceId="application" onResume={resume} />, root)); await flush(); await click('修复阅读界面');
  expect(root.textContent).toContain('用户归类暂不可用；原始记录仍可阅读，继续会话使用原工作目录。');
  expect(root.querySelector('[data-role=user]')?.textContent).toContain('你好');
  expect(resume).not.toHaveBeenCalled(); expect(fetch.mock.calls.every(([url]) => !url.includes('/launch'))).toBe(true);
  expect(Array.from(root.querySelectorAll('button')).some(button => button.textContent === '调整项目归属')).toBe(false);
  expect(root.textContent).not.toContain('项目归属已保存'); await click('继续此会话'); expect(resume).toHaveBeenCalledOnce();
});
