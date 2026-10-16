import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { DirectoryBrowser, type DirectoryListing } from './DirectoryBrowser';
import { AUTH_INVALID } from './context';

const root = document.createElement('div');
const listing: DirectoryListing = {
  instanceId: 'instance-1', currentPath: '/synthetic/projects', parentPath: '/synthetic', homePath: '/synthetic',
  roots: [{ name: '文件系统', path: '/' }, { name: '磁盘', path: '/Volumes' }],
  entries: [{ name: '中文 <script>unsafe</script>', path: '/synthetic/projects/中文 folder', isSymlink: false }],
  canSelect: true, nextCursor: null, truncated: false,
};
const ok = (value: unknown) => ({ ok: true, status: 200, json: async () => value });
const rejected = (code: string, status = 422) => ({ ok: false, status, json: async () => ({ error: { code, message: 'private-error-secret' } }) });
const deferred = <T,>() => { let resolve!: (value: T) => void; const promise = new Promise<T>(r => { resolve = r; }); return { promise, resolve }; };
const button = (text: string) => [...root.querySelectorAll('button')].find(item => item.textContent === text)!;
async function flush() { await act(async () => { await Promise.resolve(); }); }
async function mount(initialPath = '/synthetic/projects', overrides: Partial<Parameters<typeof DirectoryBrowser>[0]> = {}) {
  const props = { instanceId: 'instance-1', initialPath, onClose: vi.fn(), onSelect: vi.fn(), ...overrides };
  await act(async () => render(<DirectoryBrowser {...props} />, root)); await flush(); return props;
}
beforeEach(() => {
  document.body.append(root);
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
});
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });

it('browses the selected computer with path shortcuts and escapes names before explicit selection', async () => {
  const fetch = vi.fn().mockResolvedValue(ok(listing)); vi.stubGlobal('fetch', fetch);
  const props = await mount();
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/application/directories');
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ instanceId: 'instance-1', path: '/synthetic/projects', filter: '', showHidden: false, cursor: null });
  expect(root.querySelector('dialog')?.open).toBe(true);
  expect(root.querySelector('script')).toBeNull();
  expect(root.textContent).toContain('中文 <script>unsafe</script>');
  expect(button('磁盘')).toBeTruthy(); expect(button('上一级').disabled).toBe(false);
  expect(root.querySelector('[aria-current="location"]')?.textContent).toBe('projects');
  expect(props.onSelect).not.toHaveBeenCalled();
  await act(async () => button('选择此目录').click());
  expect(props.onSelect).toHaveBeenCalledWith('/synthetic/projects');
});

it('falls back to home only when the initial path is unavailable and explains the fallback', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(rejected('directory_forbidden')).mockResolvedValueOnce(ok({ ...listing, currentPath: '/synthetic', parentPath: '/' })); vi.stubGlobal('fetch', fetch);
  await mount('/missing'); await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(JSON.parse(fetch.mock.calls[1][1].body).path).toBeNull();
  expect(root.textContent).toContain('原目录无法访问，已返回电脑的主目录');
  fetch.mockResolvedValueOnce(rejected('directory_forbidden'));
  await act(async () => button('磁盘').click()); await flush();
  expect(fetch).toHaveBeenCalledTimes(3);
  expect(root.textContent).toContain('没有读取此目录的权限');
  expect(button('选择此目录').disabled).toBe(true);
  expect(root.textContent).not.toContain('private-error-secret');
});

it('aborts navigation and discards a late result from a previous directory', async () => {
  const first = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn().mockImplementationOnce(() => first.promise).mockResolvedValueOnce(ok({ ...listing, currentPath: '/synthetic', entries: [] })); vi.stubGlobal('fetch', fetch);
  await mount();
  const signal = fetch.mock.calls[0][1].signal as AbortSignal;
  await act(async () => button('主目录').click()); await flush();
  expect(signal.aborted).toBe(true);
  await act(async () => first.resolve(ok(listing))); await flush();
  expect(root.textContent).not.toContain('中文 <script>');
  expect(root.textContent).toContain('此目录下没有可显示的子目录');
  expect((root.querySelector('[name="directory-path"]') as HTMLInputElement).value).toBe('/synthetic');
});

it('clears all directory data and disables navigation immediately when authorization is revoked', async () => {
  const next = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn().mockResolvedValueOnce(ok(listing)).mockImplementationOnce(() => next.promise); vi.stubGlobal('fetch', fetch);
  const props = await mount();
  await act(async () => button('磁盘').click());
  const signal = fetch.mock.calls[1][1].signal as AbortSignal;
  await act(async () => { window.dispatchEvent(new Event(AUTH_INVALID)); });
  expect(signal.aborted).toBe(true);
  await act(async () => next.resolve(ok(listing))); await flush();
  expect(root.textContent).not.toContain('/synthetic'); expect(root.textContent).not.toContain('中文');
  expect(root.querySelector('input')).toBeNull(); expect(button('主目录')).toBeUndefined();
  expect(root.textContent).toContain('授权已失效'); expect(button('选择此目录').disabled).toBe(true);
  await act(async () => button('选择此目录').click()); expect(props.onSelect).not.toHaveBeenCalled();
});

it('aborts on unmount and never selects an old computer response', async () => {
  const pending = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn().mockImplementationOnce(() => pending.promise).mockResolvedValueOnce(ok({ ...listing, instanceId: 'instance-2', currentPath: '/other', entries: [] })); vi.stubGlobal('fetch', fetch);
  await mount(); const signal = fetch.mock.calls[0][1].signal as AbortSignal;
  await act(async () => render(<DirectoryBrowser key="instance-2" instanceId="instance-2" initialPath="" onClose={vi.fn()} onSelect={vi.fn()} />, root));
  expect(signal.aborted).toBe(true);
  await act(async () => pending.resolve(ok(listing))); await flush();
  expect(root.textContent).not.toContain('/synthetic'); expect(root.textContent).toContain('/other');
  await act(async () => render(null, root)); expect((fetch.mock.calls[1][1].signal as AbortSignal).aborted).toBe(true);
});

it('blocks stale-instance responses and exposes safe offline and expired-page recovery', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(ok({ ...listing, instanceId: 'another-computer' })); vi.stubGlobal('fetch', fetch);
  await mount();
  expect(root.textContent).toContain('工作台已重启'); expect(root.querySelector('input')).toBeNull();
  expect(button('选择此目录').disabled).toBe(true);
  await act(async () => render(null, root));
  fetch.mockRejectedValueOnce(new Error('private-connection-secret'));
  await mount(); expect(root.textContent).toContain('电脑可能已离线'); expect(root.textContent).not.toContain('private-');
  fetch.mockResolvedValueOnce(rejected('directory_cursor_expired'));
  await act(async () => button('重新载入目录').click()); await flush();
  expect(root.textContent).toContain('目录列表已过期'); expect(root.textContent).not.toContain('private-');
});

it('restarts bounded pages when filtering or toggling hidden directories and prevents stale selection', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn().mockResolvedValue(ok({ ...listing, nextCursor: 'next-page', truncated: true })); vi.stubGlobal('fetch', fetch);
  const props = await mount(); expect(root.textContent).toContain('列表不完整');
  await act(async () => button('下一页目录').click()); await flush();
  expect(JSON.parse(fetch.mock.calls[1][1].body).cursor).toBe('next-page');
  expect(button('返回第一页')).toBeTruthy();
  const input = root.querySelector('[name="directory-filter"]') as HTMLInputElement;
  await act(async () => { input.value = '中文'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  expect(button('选择此目录').disabled).toBe(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(200); }); await flush();
  expect(JSON.parse(fetch.mock.calls[2][1].body)).toMatchObject({ filter: '中文', cursor: null });
  const hidden = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
  await act(async () => hidden.click()); await flush();
  expect(JSON.parse(fetch.mock.calls[3][1].body)).toMatchObject({ showHidden: true, filter: '中文', cursor: null });
  expect(props.onSelect).not.toHaveBeenCalled();
});

it('returns focus to the opener, traps Tab, and cancels on Escape without focusing a mobile input', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(ok(listing)));
  const opener = document.createElement('button'); document.body.append(opener); opener.focus();
  const props = await mount(); const close = button('取消'), last = button('中文 <script>unsafe</script>›');
  expect(document.activeElement).toBe(close);
  await act(async () => { close.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true })); });
  expect(document.activeElement).toBe(last);
  await act(async () => { last.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })); });
  expect(document.activeElement).toBe(close);
  await act(async () => { close.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });
  expect(props.onClose).toHaveBeenCalledTimes(1);
  await act(async () => render(null, root)); expect(document.activeElement).toBe(opener); opener.remove();
});

it('recovers if a filter is typed and erased before its debounce completes', async () => {
  vi.useFakeTimers(); const fetch = vi.fn().mockResolvedValue(ok(listing)); vi.stubGlobal('fetch', fetch);
  await mount(); const input = root.querySelector('[name="directory-filter"]') as HTMLInputElement;
  await act(async () => { input.value = 'a'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { input.value = ''; input.dispatchEvent(new Event('input', { bubbles: true })); });
  await act(async () => { await vi.advanceTimersByTimeAsync(200); }); await flush();
  expect(button('选择此目录').disabled).toBe(false); expect(fetch).toHaveBeenCalledTimes(2);
});

it('keeps browsing available for an unselectable directory and requires edited paths to be loaded', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(ok({ ...listing, canSelect: false, selectionError: 'project_unavailable' })).mockResolvedValueOnce(ok(listing)); vi.stubGlobal('fetch', fetch);
  const props = await mount();
  expect(button('选择此目录').disabled).toBe(true); expect(root.textContent).toContain('此目录不能用于启动项目');
  await act(async () => button('主目录').click()); await flush(); expect(button('选择此目录').disabled).toBe(false);
  const input = root.querySelector('[name="directory-path"]') as HTMLInputElement;
  await act(async () => { input.value = '/not-loaded'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  expect(button('选择此目录').disabled).toBe(true); expect(props.onSelect).not.toHaveBeenCalled();
});

it('times out a stalled listing without exposing connection details', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn((_url: string, options: RequestInit) => new Promise((_resolve, reject) => options.signal?.addEventListener('abort', () => reject(new Error('private-abort-secret'))))); vi.stubGlobal('fetch', fetch);
  await mount();
  await act(async () => { await vi.advanceTimersByTimeAsync(12000); }); await flush();
  expect(root.textContent).toContain('读取目录超时'); expect(root.textContent).not.toContain('private-');
  expect(button('选择此目录').disabled).toBe(true); expect(root.querySelector('[aria-busy]')?.getAttribute('aria-busy')).toBe('false');
});
