import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { HistoryHome, type ApplicationInfo } from './HistoryHome';
const root = document.createElement('div');
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });
const initial: ApplicationInfo = { instanceId: 'application', runs: [], launchError: null, sources: [{ id: 'native', kind: 'native', state: 'unavailable', message: '原生目录不存在 <script>unsafe</script>' }] };
it('shows truthful source state with no terminal or run polling and escapes source text', async () => {
  const fetch = vi.fn(async (url: string) => ({ ok: true, json: async () => initial, text: async () => JSON.stringify({revision:'r', records:[], nextCursor:null, coverage:{state:'partial',reasons:['source_unavailable']}}) })); vi.stubGlobal('fetch', fetch);
  document.body.append(root); await act(async () => render(<HistoryHome initial={initial} />, root));
  expect(root.textContent).toContain('全部历史'); expect(root.textContent).toContain('<script>unsafe</script>');
  expect(root.querySelector('script')).toBeNull(); expect(root.querySelector('.xterm')).toBeNull();
  expect(fetch.mock.calls.every(([url]) => url === '/workbench/v1/application' || url.startsWith('/workbench/v1/library/'))).toBe(true);
  expect(root.querySelector('a')).toBeNull();
});
it('opens existing runs without a launch mutation and isolates failure from homepage', async () => {
  const info = { ...initial, launchError: 'native_launch_failed', runs: [{ runId: 'run-a', projectName: '项目 A', projectPath: '/synthetic/a', state: 'running' }] };
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => info, text: async () => JSON.stringify({revision:'r',records:[],nextCursor:null,coverage:{state:'complete_for_source',reasons:[]}}) }); vi.stubGlobal('fetch', fetch);
  document.body.append(root); await act(async () => render(<HistoryHome initial={info} />, root));
  expect(root.querySelector('a')?.getAttribute('href')).toBe('/?run=run-a');
  expect(root.textContent).toContain('Codex 未能启动'); expect(root.textContent).toContain('全部历史');
});

it.each([
  ['native_cli_not_found', '未找到可运行的 Codex CLI'],
  ['native_auth_unverified', '当前认证方式尚未通过工作台验证'],
  ['native_config_invalid', '原生 Codex 配置无法读取或解析'],
  ['private-error-with-secret', '操作未能完成，请检查连接或重新检查目录。'],
  ['constructor', '操作未能完成，请检查连接或重新检查目录。'],
])('shows safe launch reason %s and preserves the history homepage', async (code, expected) => {
  const info = { ...initial, launchError: code };
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: true, json: async () => info, text: async () => JSON.stringify({ revision: 'r', records: [], nextCursor: null, coverage: { state: 'complete_for_source', reasons: [] } }) }));
  document.body.append(root);
  await act(async () => render(<HistoryHome initial={info} />, root));
  expect(root.textContent).toContain(expected);
  expect(root.textContent).toContain('首页仍可使用。');
  expect(root.textContent).toContain('全部历史');
  expect(root.textContent).not.toContain('private-error-with-secret');
  expect(root.querySelector('#wb-settings-button')).not.toBeNull();
});

it('requires confirmation and stops only the selected run while preserving separate entry links', async () => {
  vi.useFakeTimers();
  const info = { ...initial, runs: ['a', 'b'].map(id => ({ runId: `run-${id}`, projectName: `项目 ${id}`, projectPath: `/synthetic/${id}`, state: 'running' })) };
  const fetch = vi.fn(async (_url: string, _init?: RequestInit) => ({ ok: true, json: async () => info, text: async () => JSON.stringify({revision:'r',records:[],nextCursor:null,coverage:{state:'complete_for_source',reasons:[]}}) }));
  vi.stubGlobal('fetch', fetch);
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  document.body.append(root); await act(async () => render(<HistoryHome initial={info} />, root));
  const stop = root.querySelector('[aria-label="停止项目 项目 a"]') as HTMLButtonElement;
  expect(stop.closest('a')).toBeNull();
  await act(async () => stop.click());
  expect(fetch.mock.calls.some(([, init]) => init?.method === 'POST')).toBe(false);
  confirm.mockReturnValue(true);
  await act(async () => stop.click());
  expect(fetch.mock.calls.filter(([, init]) => init?.method === 'POST')).toEqual([['/workbench/v1/runs/run-a/stop', expect.objectContaining({ body: JSON.stringify({ epoch: 'run-a' }) })]]);
  expect(root.querySelector('[aria-label="停止项目 项目 a"]')?.textContent).toBe('停止中…');
  expect((root.querySelector('[aria-label="停止项目 项目 a"]') as HTMLButtonElement).disabled).toBe(true);
  expect(root.textContent).not.toContain('已结束');
  expect(root.querySelector('[aria-label="停止项目 项目 b"]')).not.toBeNull();
  expect(root.querySelector('a[href="/?run=run-b"]')).not.toBeNull();
  info.runs[0].state = 'ended';
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  expect(root.querySelector('[aria-label="停止项目 项目 a"]')).toBeNull();
  expect(root.querySelector('a[href="/?run=run-a"]')?.textContent).toContain('已结束');
  expect(root.querySelector('[aria-label="停止项目 项目 b"]')).not.toBeNull();
});
