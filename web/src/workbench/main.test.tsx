import { afterEach, expect, it, vi } from 'vitest';

const rendered = vi.hoisted(() => ({ app: vi.fn(), home: vi.fn() }));
vi.mock('./App', () => ({ App: (props: unknown) => { rendered.app(props); return null; } }));
vi.mock('./HistoryHome', () => ({ HistoryHome: (props: unknown) => { rendered.home(props); return null; } }));
const runId = '11111111-1111-4111-8111-111111111111';
afterEach(() => { document.body.innerHTML = ''; history.replaceState(null, '', '/'); vi.unstubAllGlobals(); vi.clearAllMocks(); vi.resetModules(); });
it.each([410, 409])('points a failed device pairing back to the local network access page (%s)', async status => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', '/#pair=synthetic-token');
  const fetch = vi.fn(async () => ({ ok: false, status }));
  vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(document.body.textContent).toContain('电脑的“本地局域网接入”'));
  expect(rendered.app).not.toHaveBeenCalled();
  expect(fetch).toHaveBeenCalledOnce();
});
it.each([true, false])('pairs through the correct scope and preserves standalone compatibility (run=%s)', async scoped => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', scoped ? `/?run=${runId}#pair=test-token` : '/#pair=test-token');
  const fetch = vi.fn(async (url: string) => {
    if (url.endsWith('/pair')) return { ok: true };
    if (url.endsWith('/application')) return { ok: false, status: 404 };
    if (url.endsWith('/run')) return { ok: true, json: async () => ({ runEpoch: runId, projectName: '项目 A', terminalAvailable: true, settingsAvailable: true }) };
    throw new Error('unexpected request');
  });
  vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(rendered.app).toHaveBeenCalled());
  expect(fetch.mock.calls[0][0]).toBe(scoped ? `/workbench/v1/runs/${runId}/pair` : '/workbench/v1/pair');
  expect(fetch.mock.calls[2][0]).toBe(scoped ? `/workbench/v1/runs/${runId}/run` : '/workbench/v1/run');
  expect(rendered.app.mock.calls[0][0]).toMatchObject({ applicationHome: false, run: { settingsAvailable: !scoped } });
  expect(rendered.home).not.toHaveBeenCalled();
  expect(location.hash).toBe('');
  expect(location.pathname).toBe(scoped ? `/runs/${runId}/chat` : '/');
});

it.each(['chat', 'terminal', 'files'])('opens a paired device canonical %s document without pairing or starting again', async page => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', `/runs/${runId}/${page}`);
  const fetch = vi.fn(async (url: string) => {
    if (url === '/workbench/v1/application') return { ok: false, status: 404 };
    if (url === `/workbench/v1/runs/${runId}/run`) return { ok: true, json: async () => ({ runEpoch: runId, terminalAvailable: true, settingsAvailable: false }) };
    throw new Error('unexpected request');
  });
  vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(rendered.app).toHaveBeenCalled());
  expect(rendered.app.mock.calls.at(-1)?.[0]).toMatchObject({ applicationHome: false, page, run: { runEpoch: runId, settingsAvailable: false } });
  expect(fetch.mock.calls.map(([url]) => url)).toEqual(['/workbench/v1/application', `/workbench/v1/runs/${runId}/run`]);
  expect(location.hash).toBe('');
});

it('never displays a different run returned for a device document', async () => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', `/runs/${runId}/chat`);
  const fetch = vi.fn(async (url: string) => url.endsWith('/application') ? { ok: false, status: 404 } : {
    ok: true, json: async () => ({ runEpoch: '22222222-2222-4222-8222-222222222222', terminalAvailable: true })
  });
  vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(document.body.textContent).toContain('工作台身份不匹配'));
  expect(rendered.app).not.toHaveBeenCalled();
});

it('loads a deep-linked run once and keeps page navigation in the same run without launch requests', async () => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', `/runs/${runId}/files`);
  const fetch = vi.fn(async (url: string) => ({ ok: true, json: async () => url.endsWith('/application')
    ? { instanceId: 'app', runs: [{ runId }], sources: [], launchError: null }
    : { runEpoch: runId, projectName: '项目 A', terminalAvailable: true } }));
  vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(rendered.app).toHaveBeenCalled());
  expect(rendered.app.mock.calls.at(-1)?.[0]).toMatchObject({ page: 'files' });
  const { navigatePage } = await import('./navigation');
  navigatePage(`/runs/${runId}/terminal`);
  await vi.waitFor(() => expect(rendered.app.mock.calls.at(-1)?.[0]).toMatchObject({ page: 'terminal' }));
  history.replaceState(history.state, '', `/runs/${runId}/chat`);
  window.dispatchEvent(new PopStateEvent('popstate'));
  await vi.waitFor(() => expect(rendered.app.mock.calls.at(-1)?.[0]).toMatchObject({ page: 'chat' }));
  expect(fetch.mock.calls.map(([url]) => url)).toEqual(['/workbench/v1/application', `/workbench/v1/runs/${runId}/run`]);
});

it('rejects an unknown document before any application or run fetch', async () => {
  document.body.innerHTML = '<div id="workbench-root"></div>';
  history.replaceState(null, '', '/unknown-feature');
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  await import('./main');
  await vi.waitFor(() => expect(document.body.textContent).toContain('页面不存在'));
  expect(fetch).not.toHaveBeenCalled();
  expect(rendered.app).not.toHaveBeenCalled();
});
