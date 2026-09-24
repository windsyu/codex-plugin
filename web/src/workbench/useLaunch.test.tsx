import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { LaunchPanel, type LaunchPreview } from './LaunchPanel';
import { useLaunch } from './useLaunch';
import type { LibraryEntry } from './library';

const root = document.createElement('div');
const operationId = '11111111-1111-4111-8111-111111111111';
const preview: LaunchPreview = {
  targetId: 'target-1', canonicalPath: '/synthetic/project', configRevision: 'config-1',
  modes: ['new'], nativeHome: '/synthetic/.codex', expiresInSeconds: 30,
};

function Harness({ enter = vi.fn(), resumeEntry }: { enter?: (runId: string) => void; resumeEntry?: LibraryEntry }) {
  const launch = useLaunch('instance-1', enter);
  return <>
    <button type="button" className="choose-folder" onClick={() => void launch.pickDirectory()}>选择目录</button>
    <button type="button" onClick={() => launch.openProject('project-a')}>project-a</button>
    <button type="button" onClick={() => launch.openProject('project-b')}>project-b</button>
    {resumeEntry && <button type="button" className="resume-entry" onClick={() => launch.openResume(resumeEntry)}>继续记录</button>}
    <LaunchPanel {...launch.panel} />
  </>;
}

const ok = (value: unknown) => ({ ok: true, json: async () => value });
const rejected = (code: string, status = 400) => ({ ok: false, status, json: async () => ({ error: { code } }) });
const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
};
const button = (selector: string) => root.querySelector(selector) as HTMLButtonElement;
async function flush() { await act(async () => { await Promise.resolve(); }); }
async function validatePath(path = '/synthetic/project') {
  await act(async () => {
    const input = root.querySelector('input[aria-label="项目目录"]') as HTMLInputElement;
    input.value = path;
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await act(async () => { button('.wb-launch-validate').click(); });
  await flush();
}

beforeEach(() => {
  document.body.append(root);
  sessionStorage.clear();
  vi.stubGlobal('crypto', { randomUUID: () => operationId });
});
afterEach(() => {
  render(null, root);
  root.remove();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

it('only validates while preparing and does not create a run before explicit Start', async () => {
  const fetch = vi.fn().mockResolvedValue(ok(preview));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });

  await validatePath();

  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/launch-targets');
  expect(fetch.mock.calls.some(([url]) => url === '/workbench/v1/runs')).toBe(false);
  expect(button('.wb-launch-start').disabled).toBe(false);
});

it('resumes an unassigned entry by its identity without sending an empty or guessed project path', async () => {
  const fetch = vi.fn().mockResolvedValue(ok({ ...preview, canonicalPath: '/synthetic/Documents/Codex/2026-09-23/chat', modes: ['resume'] }));
  vi.stubGlobal('fetch', fetch);
  const entry: LibraryEntry = {
    entryId: 'unassigned', sourceId: 'native', kind: 'native', sourceRevision: 'rev-1',
    projectId: null, projectPath: null, recordedCwd: '/synthetic/Documents/Codex/2026-09-23/chat',
    title: '无项目会话', recordedAt: null, nativeThreadId: 'thread-1', runId: null,
    coverage: { state: 'complete_for_source', reasons: [] }, relatedEntryIds: [],
    capabilities: { read: true, resume: true, inspectCalls: false, manage: false },
  };
  await act(async () => { render(<Harness resumeEntry={entry} />, root); });
  await act(async () => { button('.resume-entry').click(); });
  await flush();
  expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual({ resumeEntryId: 'unassigned', sourceRevision: 'rev-1' });
  expect(root.textContent).toContain(entry.recordedCwd!);
  expect(button('.wb-launch-start').disabled).toBe(false);
  expect(fetch).toHaveBeenCalledTimes(1);
});

it('persists the operation before posting and posts one explicit Start despite a double click', async () => {
  const enter = vi.fn();
  const launch = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/launch-targets')) return ok(preview);
    if (url === '/workbench/v1/runs') {
      expect(sessionStorage.getItem('wb-launch:instance-1')).toBe(operationId);
      expect(JSON.parse(String(init?.body))).toMatchObject({ operationId, targetId: preview.targetId, configRevision: preview.configRevision, mode: 'new' });
      return launch.promise;
    }
    throw new Error(`unexpected request ${url}`);
  });
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness enter={enter} />, root); });
  await validatePath();

  await act(async () => { button('.wb-launch-start').click(); button('.wb-launch-start').click(); });

  expect(fetch.mock.calls.filter(([url, init]) => url === '/workbench/v1/runs' && init?.method === 'POST')).toHaveLength(1);
  expect(button('.wb-launch-start').disabled).toBe(true);
  expect(root.textContent).toContain('正在启动 Codex');
  await act(async () => { launch.resolve(ok({ operationId, state: 'ready', run: { runId: 'run-1' } })); });
  await flush();
  expect(enter).toHaveBeenCalledTimes(1);
  expect(enter).toHaveBeenCalledWith('run-1');
  expect(sessionStorage.getItem('wb-launch:instance-1')).toBeNull();
});

it('treats a lost create response as uncertain and only GETs that operation when asked to check', async () => {
  const fetch = vi.fn(async (url: string, _init?: RequestInit) => {
    if (url.endsWith('/launch-targets')) return ok(preview);
    if (url === '/workbench/v1/runs') throw new Error('connection lost');
    if (url === `/workbench/v1/launch-operations/${operationId}`) return ok({ operationId, state: 'starting' });
    throw new Error(`unexpected request ${url}`);
  });
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });
  await validatePath();
  await act(async () => { button('.wb-launch-start').click(); });
  await flush();

  expect(root.textContent).toContain('启动结果暂未确认');
  expect(root.querySelector('.wb-launch-start')).toBeNull();
  await act(async () => { Array.from(root.querySelectorAll('button')).find(item => item.textContent === '查询启动结果')!.click(); });
  await flush();

  expect(fetch.mock.calls.filter(([url, init]) => url === '/workbench/v1/runs' && init?.method === 'POST')).toHaveLength(1);
  expect(fetch.mock.calls.some(([url, init]) => url === `/workbench/v1/launch-operations/${operationId}` && init?.method === 'GET')).toBe(true);
});

it('restores only the saved operation after remount and offers a link without navigating or reposting', async () => {
  sessionStorage.setItem('wb-launch:instance-1', operationId);
  const first = deferred<ReturnType<typeof ok>>();
  const enter = vi.fn();
  const fetch = vi.fn()
    .mockImplementationOnce(() => first.promise)
    .mockResolvedValueOnce(ok({ operationId, state: 'ready', run: { runId: 'run-new' } }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness enter={enter} />, root); });
  await flush();
  await act(async () => { render(null, root); render(<Harness enter={enter} />, root); });
  await flush();
  await act(async () => { first.resolve(ok({ operationId, state: 'ready', run: { runId: 'run-old' } })); });
  await flush();

  expect(fetch.mock.calls.every(([url, init]) => url === `/workbench/v1/launch-operations/${operationId}` && init?.method === 'GET')).toBe(true);
  expect(enter).not.toHaveBeenCalled();
  expect(root.querySelector('a')?.getAttribute('href')).toBe('/?run=run-new');
  expect(sessionStorage.getItem('wb-launch:instance-1')).toBe(operationId);
});

it('does not recreate an expired saved operation, but lets an explicit new target clear it and recheck', async () => {
  sessionStorage.setItem('wb-launch:instance-1', operationId);
  const enter = vi.fn();
  const fetch = vi.fn()
    .mockResolvedValueOnce(rejected('operation_unavailable', 404))
    .mockResolvedValueOnce(ok({ ...preview, targetId: 'fresh-target', canonicalPath: '/synthetic/fresh' }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness enter={enter} />, root); });
  await flush();

  expect(root.textContent).toContain('这次启动记录已失效');
  expect(fetch.mock.calls).toHaveLength(1);
  expect(fetch.mock.calls[0][0]).toBe(`/workbench/v1/launch-operations/${operationId}`);
  expect(fetch.mock.calls.some(([url]) => url === '/workbench/v1/runs')).toBe(false);
  expect(enter).not.toHaveBeenCalled();

  await act(async () => { Array.from(root.querySelectorAll('button')).find(item => item.textContent === 'project-a')!.click(); });
  await flush();

  expect(fetch.mock.calls).toHaveLength(2);
  expect(fetch.mock.calls[1][0]).toBe('/workbench/v1/launch-targets');
  expect(JSON.parse(String(fetch.mock.calls[1][1]?.body))).toEqual({ projectId: 'project-a' });
  expect(root.textContent).toContain('/synthetic/fresh');
  expect(root.querySelector('.wb-launch-start')).not.toBeNull();
  expect(sessionStorage.getItem('wb-launch:instance-1')).toBeNull();
});

it('does not enter a run when the server definitively rejects Start', async () => {
  const enter = vi.fn();
  const fetch = vi.fn(async (url: string) => url.endsWith('/launch-targets') ? ok(preview) : rejected('project_changed'));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness enter={enter} />, root); });
  await validatePath();
  await act(async () => { button('.wb-launch-start').click(); });
  await flush();

  expect(enter).not.toHaveBeenCalled();
  expect(root.textContent).toContain('目录在检查后发生变化');
  expect(sessionStorage.getItem('wb-launch:instance-1')).toBeNull();
  expect(fetch.mock.calls.some(([url]) => String(url).includes('launch-operations'))).toBe(false);
});

it('discards a stale source validation when a different source is selected', async () => {
  const first = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn()
    .mockImplementationOnce(() => first.promise)
    .mockResolvedValueOnce(ok({ ...preview, targetId: 'target-b', canonicalPath: '/synthetic/b' }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });
  await act(async () => { Array.from(root.querySelectorAll('button')).find(item => item.textContent === 'project-a')!.click(); });
  await flush();
  await act(async () => { Array.from(root.querySelectorAll('button')).find(item => item.textContent === 'project-b')!.click(); });
  await flush();
  await act(async () => { first.resolve(ok({ ...preview, targetId: 'target-a', canonicalPath: '/synthetic/a' })); });
  await flush();

  expect(root.textContent).toContain('/synthetic/b');
  expect(root.textContent).not.toContain('/synthetic/a');
  expect(fetch.mock.calls.map(([, init]) => JSON.parse(String(init?.body)).projectId)).toEqual(['project-a', 'project-b']);
});

it('expires a preview and blocks Start until the target is checked again', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn().mockResolvedValue(ok({ ...preview, expiresInSeconds: 1 }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });
  await validatePath();
  expect(root.querySelector('.wb-launch-start')).not.toBeNull();
  await act(async () => { await vi.advanceTimersByTimeAsync(1000); });

  expect(root.querySelector('.wb-launch-start')).toBeNull();
  expect(root.textContent).toContain('目录检查已过期');
  expect(fetch.mock.calls.some(([url]) => url === '/workbench/v1/runs')).toBe(false);
});


it('opens one native picker, checks its exact path, and never starts a CLI automatically', async () => {
  const selection = deferred<ReturnType<typeof ok>>();
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    if (url.endsWith('/pick-directory')) {
      expect(JSON.parse(String(init?.body))).toEqual({ instanceId: 'instance-1' });
      return selection.promise;
    }
    if (url.endsWith('/launch-targets')) {
      expect(JSON.parse(String(init?.body))).toEqual({ path: '/tmp/项目 with spaces' });
      return ok(preview);
    }
    throw new Error('unexpected request');
  });
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });
  await act(async () => { button('.choose-folder').click(); button('.choose-folder').click(); });
  expect(fetch).toHaveBeenCalledTimes(1);
  await act(async () => selection.resolve(ok({ path: '/tmp/项目 with spaces' })));
  await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
  await vi.waitFor(async () => {
    await flush();
    expect(root.textContent).toContain('已确认目录');
    expect(button('.wb-launch-start').disabled).toBe(false);
  });
});

it('cancelling the native dialog preserves the selected directory without validation or startup', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(ok(preview)).mockResolvedValueOnce(ok({ path: null }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => { render(<Harness />, root); });
  await validatePath();
  await act(async () => button('.choose-folder').click());
  await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(button('.wb-launch-start').disabled).toBe(false);
  expect(root.textContent).not.toContain('未能');
});

it('picker failure exposes a manual path fallback and clears native pending state', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(rejected('picker_unavailable', 503)));
  await act(async () => { render(<Harness />, root); });
  await act(async () => button('.choose-folder').click());
  await flush();
  expect(root.textContent).toContain('直接输入路径');
  expect((root.querySelector('input') as HTMLInputElement).disabled).toBe(false);
});

it.each([
  ['run_capacity', '已达到同时运行的项目上限'],
  ['native_session_running', '这条原生会话已在另一个工作台运行'],
  ['project_already_running', '已有工作台正在运行'],
  ['native_cli_not_found', '未找到可运行的 Codex CLI'],
  ['native_auth_unverified', '当前认证方式尚未通过工作台验证'],
  ['native_project_config_invalid', '项目中的 Codex 配置无法用于本次启动'],
  ['private-error-with-secret', '操作未能完成，请检查连接或重新检查目录。'],
  ['constructor', '操作未能完成，请检查连接或重新检查目录。'],
])('shows a safe actionable message for failed launch operation %s', async (code, expected) => {
  const enter = vi.fn();
  vi.stubGlobal('fetch', vi.fn(async (url: string) => url.endsWith('/launch-targets') ? ok(preview) : ok({ operationId, state: 'failed', error: { code, message: 'private-response-secret' } })));
  await act(async () => { render(<Harness enter={enter} />, root); });
  await validatePath();
  await act(async () => { button('.wb-launch-start').click(); });
  await flush();
  expect(root.querySelector('dialog')?.textContent).toContain(expected);
  expect(root.textContent).not.toContain('private-');
  expect(enter).not.toHaveBeenCalled();
  expect(sessionStorage.getItem('wb-launch:instance-1')).toBeNull();
  expect(root.querySelector('.wb-launch-start')).toBeNull();
});

it('does not render arbitrary connection error contents while validating', async () => {
  vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('private-connection-secret')));
  await act(async () => { render(<Harness />, root); });
  await validatePath();
  expect(root.textContent).toContain('操作未能完成，请检查连接或重新检查目录。');
  expect(root.textContent).not.toContain('private-connection-secret');
});

it.each([true, false])('opens a reserved tab or offers a link when popup blocked (%s) without duplicate spawn', async blocked => {
  const replace = vi.fn(), close = vi.fn();
  const popup = { closed: false, location: { href: 'about:blank', replace }, close, opener: window };
  const open = vi.spyOn(window, 'open').mockReturnValue(blocked ? null : popup as unknown as Window);
  const enter = vi.fn();
  const fetch = vi.fn(async (url: string) => url.endsWith('/launch-targets') ? ok({ ...preview, openInNewTab: true }) : ok({ operationId, state: 'ready', openInNewTab: true, run: { runId: 'run-b' } }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(<Harness enter={enter} />, root));
  await validatePath();
  await act(async () => button('.wb-launch-start').click());
  await flush();
  expect(open).toHaveBeenCalledWith('about:blank', '_blank');
  expect(enter).not.toHaveBeenCalled();
  expect(root.querySelector('a')?.getAttribute('href')).toBe('/?run=run-b');
  expect(replace).toHaveBeenCalledTimes(blocked ? 0 : 1);
  expect(fetch.mock.calls.filter(([url]) => url.endsWith('/runs'))).toHaveLength(1);
  expect(root.querySelector('.wb-launch-start')).toBeNull();
});
it('closes its reserved blank tab on definitive launch failure', async () => {
  const close = vi.fn();
  vi.spyOn(window, 'open').mockReturnValue({ closed: false, location: { href: 'about:blank' }, close } as unknown as Window);
  vi.stubGlobal('fetch', vi.fn(async (url: string) => url.endsWith('/launch-targets') ? ok({ ...preview, openInNewTab: true }) : rejected('project_changed')));
  await act(async () => render(<Harness />, root));
  await validatePath();
  await act(async () => button('.wb-launch-start').click());
  await flush();
  expect(close).toHaveBeenCalledTimes(1);
});

it('uses the reserved tab even when other runs end before the launch result', async () => {
  const replace = vi.fn(), enter = vi.fn();
  vi.spyOn(window, 'open').mockReturnValue({ closed: false, location: { href: 'about:blank', replace } } as unknown as Window);
  vi.stubGlobal('fetch', vi.fn(async (url: string) => url.endsWith('/launch-targets') ? ok({ ...preview, openInNewTab: true }) : ok({ operationId, state: 'ready', openInNewTab: false, run: { runId: 'run-b' } })));
  await act(async () => render(<Harness enter={enter} />, root));
  await validatePath();
  await act(async () => button('.wb-launch-start').click());
  await flush();
  expect(replace).toHaveBeenCalledWith('/?run=run-b');
  expect(enter).not.toHaveBeenCalled();
});

it.each([true, false])('opens an existing run in a new tab without spawning, with a fallback link (blocked=%s)', async blocked => {
  const enter = vi.fn();
  const open = vi.spyOn(window, 'open').mockReturnValue(blocked ? null : {} as Window);
  const fetch = vi.fn().mockResolvedValue(ok({ ...preview, openInNewTab: true, existingRun: { runId: 'run-b', projectName: '项目 B', state: 'running' } }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(<Harness enter={enter} />, root));
  await validatePath();
  await act(async () => { Array.from(root.querySelectorAll('button')).find(item => item.textContent === '进入工作台')!.click(); });
  expect(open).toHaveBeenCalledWith('/?run=run-b', '_blank', 'noopener');
  expect(enter).not.toHaveBeenCalled();
  expect(root.querySelector('a')?.getAttribute('href')).toBe('/?run=run-b');
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/launch-targets');
  expect(root.querySelector('.wb-launch-start')).toBeNull();
});
