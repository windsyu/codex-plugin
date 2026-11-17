import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { WorkbenchFrame, useApplicationStatus } from './WorkbenchFrame';
import type { ApplicationInfo } from './HistoryHome';
import { runPath } from './routes';

const access = vi.hoisted(() => ({ gateway: undefined as string | undefined, assert: vi.fn() }));
vi.mock('./context', () => ({
  workbenchContext: () => access,
  workbenchFetch: (url: string, init?: RequestInit) => fetch(url, init),
  navigationURL: (path: string) => path
}));
const root = document.createElement('div');
const epoch = '11111111-1111-4111-8111-111111111111';
const gateway = '22222222-2222-4222-8222-222222222222';
const application: ApplicationInfo = { instanceId: 'application', runs: [{ runId: epoch, projectName: '项目 A', projectPath: '/synthetic/a', state: 'running' }], sources: [], launchError: null, launchAvailable: true };
const run = { runEpoch: epoch, projectName: '项目 A', processId: 42, terminalAvailable: true, historyAvailable: true, accessAvailable: true, settingsAvailable: true };
const response = (value: unknown) => ({ ok: true, status: 200, json: async () => value }) as Response;
const frame = (info: ApplicationInfo | null = application) => <WorkbenchFrame application={info} run={run} route={{ kind: 'run', runId: epoch, page: 'chat' }}><p>工作区</p></WorkbenchFrame>;
beforeEach(() => {
  document.body.append(root); access.gateway = undefined; access.assert.mockReset();
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });

it('shows local computer metadata safely and only links to a configured HTTPS portal root', async () => {
  const unsafe = '<img src=x onerror=alert(1)>';
  const fetch = vi.fn(async (_url: string, _init?: RequestInit) => response({ instanceId: 'application', relay: { name: unsafe, state: 'online', serverUrl: 'https://relay.example.com' } }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(frame(), root));
  await act(async () => {});
  expect(root.textContent).toContain(unsafe);
  expect(root.querySelector('img')).toBeNull();
  expect(root.querySelector('a[href="https://relay.example.com/computers"]')?.getAttribute('rel')).toBe('noopener noreferrer');
  expect(root.querySelector('a[href="/relay"]')).not.toBeNull();
  expect(root.textContent).toContain('公网已开启');
  expect(fetch).toHaveBeenCalledOnce();
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/application/relay');
});

it.each(['http://relay.example.com', 'https://user@relay.example.com', 'https://relay.example.com/path', 'https://relay.example.com?token=private', 'javascript:alert(1)'])('does not turn invalid portal metadata into a navigation link (%s)', async serverUrl => {
  vi.stubGlobal('fetch', vi.fn(async () => response({ instanceId: 'application', relay: { name: 'metadata-ready', state: 'disabled', serverUrl } })));
  await act(async () => render(frame(), root));
  await act(async () => {});
  expect(root.textContent).toContain('metadata-ready');
  expect([...root.querySelectorAll('.wb-product-nav a')].some(link => link.textContent?.includes('我的电脑'))).toBe(false);
  expect(root.querySelector('a[href="/relay"]')).not.toBeNull();
});

it('keeps a remote owner out of local connector management even without optional portal metadata', async () => {
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  await act(async () => render(frame({ ...application, remoteOwner: true }), root));
  expect(fetch).not.toHaveBeenCalled();
  expect(root.querySelector('a[href="/relay"]')).toBeNull();
  expect(root.querySelector(`a[href="${runPath(epoch, 'access')}"]`)).not.toBeNull();
  expect(root.querySelector('a[href="/launch"]')).not.toBeNull();
  expect(root.querySelector('a[href="/computers"]')).not.toBeNull();
});

it('labels the pinned remote computer rather than another account computer and discards an invalidated reply', async () => {
  access.gateway = gateway;
  let complete: (value: Response) => void = () => {};
  const fetch = vi.fn((_url: string, _init?: RequestInit) => new Promise<Response>(resolve => { complete = resolve; }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(frame({ ...application, remoteOwner: true }), root));
  expect(fetch.mock.calls[0][0]).toBe('/relay/v1/gateways');
  await act(async () => complete(response({ gateways: [{ gatewayId: 'another', name: '另一台电脑' }, { gatewayId: gateway, name: '固定授权电脑' }] })));
  await act(async () => {});
  expect(root.textContent).toContain('固定授权电脑');
  expect(root.textContent).not.toContain('另一台电脑');
  await act(async () => render(null, root));
  access.assert.mockImplementation(() => { throw new Error('expired'); });
  await act(async () => render(frame({ ...application, remoteOwner: true }), root));
  await act(async () => complete(response({ gateways: [{ gatewayId: gateway, name: '迟到的旧授权数据' }] })));
  await act(async () => {});
  expect(root.textContent).not.toContain('迟到的旧授权数据');
  expect(root.querySelector('a[href="/relay"]')).toBeNull();
});

it('keeps paired navigation within its Run capabilities without any application metadata query', async () => {
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  const paired = { ...run, accessAvailable: false, settingsAvailable: false };
  await act(async () => render(<WorkbenchFrame application={null} route={{ kind: 'run', runId: epoch, page: 'chat' }} run={paired}><p>当前运行</p></WorkbenchFrame>, root));
  expect(fetch).not.toHaveBeenCalled();
  const hrefs = [...root.querySelectorAll('a')].map(link => link.getAttribute('href'));
  expect(hrefs.every(path => path?.startsWith(`/runs/${epoch}/`))).toBe(true);
  expect(hrefs).toContain(runPath(epoch, 'history'));
  expect(hrefs).toContain(runPath(epoch, 'usage'));
  expect(hrefs).not.toContain(runPath(epoch, 'access'));
  expect(root.textContent).toContain('仅当前运行');
});

it('keeps one sidebar when moving mobile navigation into and out of the modal', async () => {
  vi.stubGlobal('innerWidth', 390);
  vi.stubGlobal('fetch', vi.fn(async () => response({ instanceId: 'application', relay: { state: 'disabled' } })));
  await act(async () => render(frame(), root));
  const sidebar = root.querySelector('.wb-project-navigation')!;
  const toggle = root.querySelector('.wb-navigation-toggle') as HTMLButtonElement;
  await act(async () => { toggle.focus(); toggle.click(); });
  expect(root.querySelector('dialog[open] .wb-project-navigation')).toBe(sidebar);
  const query = root.querySelector('input') as HTMLInputElement;
  await act(async () => { query.value = '项目'; query.dispatchEvent(new Event('input', { bubbles: true })); });
  expect(root.querySelectorAll('.wb-project-navigation')).toHaveLength(1);
  expect(root.querySelector('dialog[open] .wb-project-navigation')).toBe(sidebar);
  await act(async () => { root.querySelector('dialog')!.dispatchEvent(new Event('cancel', { cancelable: true })); });
  expect(root.querySelector('.wb-navigation-desktop .wb-project-navigation')).toBe(sidebar);
  expect(query.value).toBe('项目');
  expect(document.activeElement).toBe(toggle);
});

it('shares bootstrap state, serializes polling, rejects a different application and aborts on unmount', async () => {
  vi.useFakeTimers();
  let complete: (value: Response) => void = () => {};
  const fetch = vi.fn((_url: string, _init?: RequestInit) => new Promise<Response>(resolve => { complete = resolve; }));
  vi.stubGlobal('fetch', fetch);
  function Status() { const state = useApplicationStatus(application); return <p>{state.info?.instanceId} / {state.info?.runs[0]?.state} / {state.error}</p>; }
  await act(async () => render(<Status />, root));
  expect(fetch).not.toHaveBeenCalled();
  await act(async () => { vi.advanceTimersByTime(2000); });
  expect(fetch).toHaveBeenCalledOnce();
  await act(async () => { vi.advanceTimersByTime(10000); });
  expect(fetch).toHaveBeenCalledOnce();
  await act(async () => complete(response({ ...application, instanceId: 'another', runs: [] })));
  await act(async () => {});
  expect(root.textContent).toContain('应用已重新启动');
  expect(root.textContent).toContain('application / running');
  await act(async () => { vi.advanceTimersByTime(2000); });
  expect(fetch).toHaveBeenCalledTimes(2);
  const signal = fetch.mock.calls[1][1]?.signal;
  await act(async () => render(null, root));
  expect(signal?.aborted).toBe(true);
  await act(async () => complete(response(application)));
  await act(async () => { vi.advanceTimersByTime(10000); });
  expect(fetch).toHaveBeenCalledTimes(2);
});
