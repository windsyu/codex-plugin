import { afterEach, expect, it, vi } from 'vitest';

const rendered = vi.hoisted(() => ({ app: vi.fn(), home: vi.fn() }));
vi.mock('./App', () => ({ App: (props: unknown) => { rendered.app(props); return null; } }));
vi.mock('./HistoryHome', () => ({ HistoryHome: (props: unknown) => { rendered.home(props); return null; } }));
const runId = '11111111-1111-4111-8111-111111111111';
afterEach(() => { document.body.innerHTML = ''; history.replaceState(null, '', '/'); vi.unstubAllGlobals(); vi.clearAllMocks(); vi.resetModules(); });
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
});
