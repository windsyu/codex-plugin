import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { SessionGroupList } from './SessionGroupList';
import { readSessionGroup, type GroupMember, type SessionGroup } from './library';
const root = document.createElement('div');
const member: GroupMember = { entryId: 'e_native', sourceId: 'native', sourceIdentity: 'identity', kind: 'native', sourceRevision: 'n-v', title: '原生标题', coverage: { state: 'complete_for_source', reasons: [] }, capabilities: { read: true, resume: true, inspectCalls: false, manage: false } };
const observer: GroupMember = { ...member, entryId: 'e_old', sourceId: 'old', sourceIdentity: 'old-identity', kind: 'observer', sourceRevision: 'old-v', title: '旧来源', match: { record: 9, text: '仅旧库命中', sourceRevision: 'old-v' } };
const group: SessionGroup = { groupId: 'g', section: 'primary', conversationKind: 'ordinary', preferredEntryId: observer.entryId, members: [observer], projectId: 'p', projectPath: '/synthetic/project', latestRecordedAt: null };
const sources = [{ id: 'native', identity: 'identity' }, { id: 'old', identity: 'old-identity' }];
async function flush() { await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)); }); }
async function mount(onOpen = vi.fn(), value = group) { document.body.append(root); await act(async () => render(<SessionGroupList groups={[value]} revision="catalog-v" sources={sources} onOpen={onOpen} />, root)); await flush(); return onOpen; }
async function click(text: string) { const button = Array.from(root.querySelectorAll('button')).find(value => value.textContent?.includes(text)); expect(button).toBeTruthy(); await act(async () => button!.click()); await flush(); }
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
it('uses minimal member identity and its own search location without fetching other sources', async () => {
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch); const onOpen = await mount();
  await click('旧来源'); expect(onOpen).toHaveBeenCalledWith(observer); expect(fetch).not.toHaveBeenCalled();
  expect(root.textContent).not.toContain('继续此会话');
});
it('puts a readable task and its own latest exchange in the row, keeping technical fields collapsed', async () => {
  const value: SessionGroup = { ...group, conversationKind: 'unknown', preferredEntryId: member.entryId, members: [{ ...member, title: '将 V2 打包发布', preview: { text: '安装包与快速启动验证已补齐。', role: 'assistant' } }] };
  await mount(vi.fn(), value);
  const row = root.querySelector('.wb-library-entry')!;
  expect(row.textContent).toContain('将 V2 打包发布');
  expect(row.textContent).toContain('安装包与快速启动验证已补齐。');
  expect(row.textContent).not.toContain('类型未确认');
  expect(row.textContent).not.toContain('原生会话');
  expect(row.textContent).not.toContain('/synthetic/project');
  const information = root.querySelector<HTMLDetailsElement>('.wb-library-entry-info');
  expect(information?.open).toBe(false);
  expect(information?.textContent).toContain('类型未确认');
  expect(information?.textContent).toContain('/synthetic/project');
});
it('does not borrow the latest exchange from another source or invent a completion state', async () => {
  await mount(vi.fn(), { ...group, members: [observer, { ...member, preview: { text: '另一来源的回复', role: 'assistant' } }] });
  const row = root.querySelector('.wb-library-entry')!;
  expect(row.textContent).not.toContain('另一来源的回复');
  expect(row.textContent).not.toContain('已完成');
});
it('loads all related metadata only on explicit click without inheriting match or filters', async () => {
  const fetch = vi.fn(async (_url: string) => ({ ok: true, text: async () => JSON.stringify({ ...group, members: [observer, member] }) })); vi.stubGlobal('fetch', fetch);
  const onOpen = await mount(); expect(fetch).not.toHaveBeenCalled(); await click('查看全部关联来源');
  const url = new URL(fetch.mock.calls[0][0] as string, 'http://localhost');
  expect(url.pathname).toBe('/workbench/v1/library/session-groups/g');
  expect([...url.searchParams.keys()].sort()).toEqual(['anchorEntryId', 'revision']);
  expect(url.searchParams.get('anchorEntryId')).toBe(observer.entryId);
  expect(url.searchParams.get('revision')).toBe('catalog-v');
  await click('原生标题'); expect(onOpen).toHaveBeenCalledWith({ ...member, match: undefined });
  expect(root.textContent).toContain('不改变列表筛选或计数');
});
it('explains an expired relation without silently switching reading source', async () => {
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false, status: 409, json: async () => ({ error: 'stale_cursor' }) })));
  const onOpen = await mount(); await click('查看全部关联来源');
  expect(root.querySelector('[role=alert]')?.textContent).toContain('重新读取列表'); expect(onOpen).not.toHaveBeenCalled();
});
it('removes revoked source summaries and aborts relation reads', async () => {
  let requestSignal: AbortSignal | null | undefined;
  vi.stubGlobal('fetch', vi.fn(async (_url: string, options: RequestInit) => { requestSignal = options.signal; return await new Promise(() => {}); }));
  const onOpen = await mount(); await click('查看全部关联来源');
  await act(async () => render(<SessionGroupList groups={[group]} revision="catalog-v" sources={[]} onOpen={onOpen} />, root));
  expect(requestSignal?.aborted).toBe(true); expect(root.querySelector('button')).toBeNull();
});
it('caps related summaries at ten members and never substitutes another group or anchor', async () => {
  for (const response of [
    { ...group, members: Array.from({ length: 11 }, (_, index) => ({ ...observer, entryId: index === 0 ? observer.entryId : `e_${index}` })) },
    { ...group, groupId: 'other' },
    { ...group, preferredEntryId: member.entryId, members: [member] },
  ]) {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, text: async () => JSON.stringify(response) })));
    await expect(readSessionGroup('g', observer.entryId, 'catalog-v', new AbortController().signal)).rejects.toThrow();
  }
});
it('reads the actual bare SessionGroup endpoint contract with its catalog revision in the request', async () => {
  const fetch = vi.fn(async (_url: string) => ({ ok: true, text: async () => JSON.stringify(group) })); vi.stubGlobal('fetch', fetch);
  await expect(readSessionGroup('g', observer.entryId, 'catalog-v', new AbortController().signal)).resolves.toEqual(group);
  const request = new URL(fetch.mock.calls[0][0], 'http://localhost');
  expect(request.searchParams.get('revision')).toBe('catalog-v');
  expect(request.searchParams.get('anchorEntryId')).toBe(observer.entryId);
});
