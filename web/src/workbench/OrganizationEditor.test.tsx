import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { OrganizationEditor } from './OrganizationEditor';
import type { LibraryEntry, Organization, OrganizationResponse } from './library';
const root = document.createElement('div');
const entry: LibraryEntry = { entryId: 'e', sourceId: 'native', sourceIdentity: 'source-identity', sourceRevision: 'source-v', kind: 'native', projectId: 'p_old', projectPath: '/original/project', recordedCwd: '/original/execution', title: '合成会话', recordedAt: null, nativeThreadId: 'thread', runId: null, coverage: { state: 'complete_for_source', reasons: [] }, capabilities: { read: true, resume: true, inspectCalls: false, manage: false }, relatedEntryIds: [] };
const automatic: Organization = { automatic: { projectId: 'p_old', projectPath: '/original/project', basis: 'cwd_inferred' }, effective: { projectId: 'p_old', projectPath: '/original/project', basis: 'automatic' }, unassignedReason: null, conversationKind: 'ordinary', kindBasis: 'source_known', overrideState: 'none' };
const state = (organization = automatic, revision = 'absent', canAdjust = true): OrganizationResponse => ({ organization, organizationRevision: revision, revision: `catalog-${revision}`, canAdjust });
const response = (value: unknown) => ({ ok: true, text: async () => JSON.stringify(value) });
async function flush() { await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)); }); }
async function mount(handler?: (url: URL, request: RequestInit) => unknown, initial = state()) {
  const fetch = vi.fn(async (url: string, request: RequestInit) => {
    const location = new URL(url, 'http://localhost'); const custom = handler?.(location, request); if (custom !== undefined) return custom;
    return location.pathname.endsWith('/projects') ? response({ revision: 'catalog-v', records: [{ projectId: 'p_target', path: '/target/project', entries: 1 }], nextCursor: null, coverage: entry.coverage }) : response(initial);
  }); vi.stubGlobal('fetch', fetch); const onSaved = vi.fn(), onClose = vi.fn(); document.body.append(root);
  await act(async () => render(<OrganizationEditor entry={entry} instanceId="app" onSaved={onSaved} onClose={onClose} />, root)); await flush(); await flush();
  return { fetch, onSaved, onClose };
}
async function click(text: string) { const button = Array.from(root.querySelectorAll('button')).find(value => value.textContent?.includes(text)); expect(button).toBeTruthy(); await act(async () => button!.click()); await flush(); }
async function action(value: string) { await act(async () => { const select = root.querySelector<HTMLSelectElement>('[aria-label="调整方式"]')!; select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })); }); await flush(); }
async function confirm() { await act(async () => { const input = root.querySelector<HTMLInputElement>('input[type=checkbox]')!; input.checked = true; input.dispatchEvent(new Event('change', { bubbles: true })); }); await flush(); }
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
it('requires explicit target and confirmation and sends only trusted IDs with quoted CAS revision', async () => {
  const result = state({ ...automatic, effective: { projectId: 'p_target', projectPath: '/target/project', basis: 'user_assigned' }, overrideState: 'applied' }, 'store:1');
  const { fetch, onSaved } = await mount((_url, request) => request.method === 'PUT' ? response(result) : undefined);
  expect(fetch.mock.calls.filter(([, request]) => request.method === 'PUT')).toHaveLength(0);
  expect(root.textContent).toContain('继续此会话仍使用原工作目录：/original/execution');
  expect(Array.from(root.querySelectorAll('button')).find(button => button.textContent === '保存归属')?.disabled).toBe(true);
  await click('/target/project'); await confirm(); await click('保存归属');
  const save = fetch.mock.calls.find(([, request]) => request.method === 'PUT')![1];
  expect(save.headers).toMatchObject({ 'If-Match': '"absent"' });
  expect(JSON.parse(save.body as string)).toEqual({ instanceId: 'app', sourceIdentity: 'source-identity', sourceRevision: 'source-v', action: 'project', projectId: 'p_target' });
  expect(onSaved).toHaveBeenCalledWith(result);
});
it.each(['unassigned', 'automatic'])('supports %s without a client-supplied path or project ID', async value => {
  const { fetch } = await mount(); await action(value); await confirm(); await click('保存归属');
  const body = JSON.parse(fetch.mock.calls.find(([, request]) => request.method === 'PUT')![1].body as string);
  expect(body.action).toBe(value); expect(body).not.toHaveProperty('projectId'); expect(body).not.toHaveProperty('path');
});
it('never presents unavailable organization as automatic and prevents writes', async () => {
  const { fetch } = await mount(() => ({ ok: false, status: 503, json: async () => ({ error: 'organization_unavailable' }) }));
  expect(root.textContent).toContain('用户归类暂不可用'); expect(root.querySelector('[aria-label="调整方式"]')).toBeNull();
  expect(root.textContent).not.toContain('自动归属：'); expect(fetch.mock.calls.every(([, request]) => request.method !== 'PUT')).toBe(true);
});
it('explains reliable-identity refusal without permitting a save', async () => {
  await mount(undefined, { ...state(automatic, 'absent', false), adjustmentReason: '记录身份无法核验' });
  expect(root.textContent).toContain('记录身份无法核验'); expect(root.querySelector('[aria-label="调整方式"]')).toBeNull();
});
it.each([409, 'lost-ack'])('reconciles %s by GET and never blindly repeats a PUT', async failure => {
  let current = state();
  const committed = state({ ...automatic, effective: { projectId: null, projectPath: null, basis: 'user_unassigned' }, overrideState: 'applied', unassignedReason: 'user_unassigned' }, 'store:2');
  const { fetch, onSaved } = await mount((_url, request) => {
    if (request.method === 'PUT') { current = committed; return failure === 409 ? { ok: false, status: 409, json: async () => ({ error: 'organization_changed' }) } : Promise.reject(new Error('lost acknowledgement')); }
    if (_url.pathname.endsWith('/organization')) return response(current);
  });
  await action('unassigned'); await confirm(); await click('保存归属');
  expect(root.textContent).toContain('未能确认保存结果'); expect(onSaved).not.toHaveBeenCalled();
  expect(root.querySelector('[aria-label="调整方式"]')).toBeNull();
  await click('重新核对当前归类');
  expect(root.textContent).toContain('没有自动重试保存'); expect(onSaved).toHaveBeenCalledWith(committed);
  expect(fetch.mock.calls.filter(([, request]) => request.method === 'PUT')).toHaveLength(1);
});
it('aborts pending metadata and source requests when closed', async () => {
  let signal: AbortSignal | null | undefined;
  const { onClose } = await mount((_url, request) => { signal = request.signal; return new Promise(() => {}); });
  await click('关闭'); expect(signal?.aborted).toBe(true); expect(onClose).toHaveBeenCalledOnce();
});
it('keeps a failed target lookup retryable and only searches the bounded project directory', async () => {
  let projectReads = 0;
  const { fetch } = await mount((url) => url.pathname.endsWith('/projects') && ++projectReads === 1 ? { ok: false, status: 503 } : undefined);
  expect(root.textContent).toContain('重新读取目标项目');
  await click('重新读取目标项目');
  expect(root.textContent).toContain('/target/project');
  await act(async () => { const input = root.querySelector<HTMLInputElement>('[aria-label="搜索目标项目"]')!; input.value = 'synthetic'; input.dispatchEvent(new Event('input', { bubbles: true })); }); await click('查找目标项目');
  const url = new URL(fetch.mock.calls.filter(([url]) => new URL(url, 'http://localhost').pathname.endsWith('/projects')).at(-1)![0], 'http://localhost');
  expect(url.searchParams.get('projectQ')).toBe('synthetic'); expect(url.searchParams.get('limit')).toBe('30'); expect(url.searchParams.has('q')).toBe(false);
  expect(fetch.mock.calls.every(([, request]) => request.method !== 'PUT')).toBe(true);
});
it('refuses malformed organization rather than presenting a writable automatic fallback', async () => {
  const { fetch } = await mount(() => response({ ...state(), organization: { ...automatic, effective: { ...automatic.effective, basis: 'fabricated' } } }));
  expect(root.textContent).toContain('用户归类暂不可用'); expect(root.querySelector('[aria-label="调整方式"]')).toBeNull();
  expect(fetch.mock.calls.every(([, request]) => request.method !== 'PUT')).toBe(true);
});
it('allows keyboard cancel to abort and close without mutation', async () => {
  const { fetch, onClose } = await mount();
  const event = new Event('cancel', { bubbles: false, cancelable: true });
  await act(async () => { root.querySelector('dialog')!.dispatchEvent(event); });
  expect(event.defaultPrevented).toBe(true); expect(onClose).toHaveBeenCalledOnce(); expect(fetch.mock.calls.every(([, request]) => request.method !== 'PUT')).toBe(true);
});
