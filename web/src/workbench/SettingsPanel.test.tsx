import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useState } from 'preact/hooks';
import { SettingsPanel, type Settings, type WorkbenchConfig } from './SettingsPanel';

const config: WorkbenchConfig = { schemaVersion: 1, launch: { openBrowser: true, codexBin: 'codex', profile: null, providerProfile: 'unmanaged-custom' }, storage: { dataDir: null }, history: { cleanup: { enabled: false, retention: { enabled: false, days: 90 } } } };
const data = (): Settings => ({ schemaVersion: 1, revision: 'a'.repeat(64), saved: structuredClone(config), effective: structuredClone(config), defaults: structuredClone(config), cliOverrides: [], configPath: '/synthetic/config.json', effectiveDataDir: '/synthetic/history', restartRequired: [], errors: [], capabilities: { manualCleanup: false, retention: false } });
const ok = (settings = data()) => ({ ok: true, status: 200, json: async () => ({ currentRunEpoch: 'live', settings }) });
const fail = (code: string, field?: string) => ({ ok: false, json: async () => ({ error: { code, field } }) });
const root = document.createElement('div');
const button = (text: string) => [...root.querySelectorAll('button')].find(b => b.textContent === text)!;
const input = (text: string) => root.querySelector(`[aria-label="${text}"]`) as HTMLInputElement;
async function click(text: string) { await act(async () => button(text).click()); await act(async () => {}); }
async function submit() {
  await act(async () => { root.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); });
  await act(async () => {});
}
async function type(text: string, value: string) { await act(async () => { const field = input(text); field.value = value; field.dispatchEvent(new Event('input', { bubbles: true })); }); }
async function toggle(text: string) { await act(async () => input(text).click()); }
function Harness() {
  const [open, setOpen] = useState(false); const [dirty, setDirty] = useState(false);
  return <><button id="wb-settings-button" onClick={() => setOpen(!open)}>设置</button><span data-dirty>{String(dirty)}</span><textarea aria-label="native" /><SettingsPanel epoch="live" open={open} onClose={() => setOpen(false)} onDirtyChange={setDirty} /></>;
}
beforeEach(() => document.body.append(root));
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
async function show() { await act(async () => render(<Harness />, root)); }

it('loads only when expanded and preserves unsaved fields across collapse without writing', async () => {
  const fetch = vi.fn().mockResolvedValue(ok()); vi.stubGlobal('fetch', fetch); await show();
  expect(fetch).not.toHaveBeenCalled(); const native = input('native');
  await click('设置'); expect(input('允许清理历史记录').disabled).toBe(true); expect(input('保留天数').disabled).toBe(true);
  await type('Codex 启动配置', 'future'); expect(root.querySelector('[data-dirty]')!.textContent).toBe('true');
  await click('收起'); expect((root.querySelector('#wb-settings-panel') as HTMLElement).hidden).toBe(true);
  await click('设置'); expect(input('Codex 启动配置').value).toBe('future'); expect(input('native')).toBe(native);
  expect(fetch.mock.calls.every(([, init]) => !init.method)).toBe(true);
  await act(async () => { input('Codex 启动配置').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); });
  expect((root.querySelector('#wb-settings-panel') as HTMLElement).hidden).toBe(true);
});
it('writes the same full JSON with revision and preserves current effective settings', async () => {
  const fetch = vi.fn().mockImplementation(async (_url, init) => {
    if (!init.method) return ok();
    const next = data(); next.saved = JSON.parse(init.body).config; next.revision = 'b'.repeat(64); next.restartRequired = ['launch.profile']; return ok(next);
  }); vi.stubGlobal('fetch', fetch); await show(); await click('设置');
  await type('Codex 启动配置', 'future');
  await submit();
  const call = fetch.mock.calls.find(([, init]) => init.method === 'PUT')!;
  expect(call[0]).toBe('/workbench/v1/settings'); expect(call[1].headers['If-Match']).toBe(`"${'a'.repeat(64)}"`);
  expect(JSON.parse(call[1].body)).toEqual({ currentRunEpoch: 'live', config: { ...config, launch: { ...config.launch, profile: 'future' } } });
  expect(root.textContent).toContain('待下次启动生效：Codex 启动配置');
  expect(root.querySelector('[data-dirty]')!.textContent).toBe('false');
  expect((root.querySelector('#wb-settings-panel') as HTMLElement).hidden).toBe(false);
});
it('keeps conflict drafts, requires explicit reload, and treats defaults as a draft', async () => {
  let server = data();
  const fetch = vi.fn().mockImplementation(async (_url, init) => init.method ? fail('config_changed') : ok(server));
  vi.stubGlobal('fetch', fetch); await show(); await click('设置'); await type('Codex 启动配置', 'draft');
  await submit();
  expect(input('Codex 启动配置').value).toBe('draft'); expect(button('保存').disabled).toBe(true);
  server = { ...data(), revision: 'b'.repeat(64), saved: { ...config, launch: { ...config.launch, profile: 'external' } } };
  await click('重新加载（放弃草稿）'); expect(input('Codex 启动配置').value).toBe('external');
  await click('恢复默认值'); expect(input('Codex 启动配置').value).toBe(''); expect(root.querySelector('[data-dirty]')!.textContent).toBe('true');
  await click('取消修改'); expect(input('Codex 启动配置').value).toBe('external');
  expect(fetch.mock.calls.filter(([, init]) => init.method === 'PUT')).toHaveLength(1);
});
it('allows correcting a rejected field without discarding the draft', async () => {
  const fetch = vi.fn().mockImplementation(async (_url, init) => init.method ? fail('invalid_field', 'storage.dataDir') : ok());
  vi.stubGlobal('fetch', fetch); await show(); await click('设置'); await type('历史保存位置', 'relative');
  await submit();
  expect(root.textContent).toContain('历史保存位置：填写的内容不符合要求');
  await type('历史保存位置', '/synthetic/new'); expect(button('保存').disabled).toBe(false);
});
it('saves cleanup policy without losing collapsed startup values and disables retention with cleanup', async () => {
  const server = data(); server.capabilities = { manualCleanup: true, retention: true };
  server.saved!.launch = { openBrowser: false, codexBin: '/synthetic/codex', profile: 'custom', providerProfile: 'unmanaged-custom' };
  const fetch = vi.fn().mockImplementation(async (_url, init) => {
    if (init.method) { server.saved = JSON.parse(init.body).config; server.revision = 'b'.repeat(64); }
    return ok(server);
  });
  vi.stubGlobal('fetch', fetch); await show(); await click('设置');
  expect(input('Codex 启动配置').closest('details')!.open).toBe(false);
  expect(root.querySelector('select')).toBeNull();
  expect(root.textContent).not.toContain(server.configPath);
  expect(input('自动清理过期记录').disabled).toBe(true);
  await toggle('允许清理历史记录'); expect(input('自动清理过期记录').disabled).toBe(false);
  await toggle('自动清理过期记录'); expect(input('保留天数').disabled).toBe(false);
  await type('保留天数', '30');
  expect(fetch.mock.calls.every(([, init]) => !init.method)).toBe(true);
  const launch = structuredClone(server.saved!.launch);
  await submit();
  const call = fetch.mock.calls.find(([, init]) => init.method === 'PUT')!;
  expect(JSON.parse(call[1].body).config).toEqual({ ...config, launch, history: { cleanup: { enabled: true, retention: { enabled: true, days: 30 } } } });
  await toggle('允许清理历史记录');
  expect(input('自动清理过期记录').checked).toBe(false);
  expect(input('自动清理过期记录').disabled).toBe(true); expect(input('保留天数').disabled).toBe(true);
  expect(fetch.mock.calls.filter(([, init]) => init.method === 'PUT')).toHaveLength(1);
});
it('keeps actual history and log locations after saving a future directory and safely renders paths', async () => {
  const server = data(); server.effectiveDataDir = '/synthetic/<img src=x>/history/'; server.cliOverrides = ['storage.dataDir'];
  const fetch = vi.fn().mockImplementation(async (_url, init) => {
    if (init.method) { server.saved = JSON.parse(init.body).config; server.revision = 'b'.repeat(64); server.restartRequired = ['storage.dataDir']; }
    return ok(server);
  });
  vi.stubGlobal('fetch', fetch); await show(); await click('设置');
  await type('历史保存位置', '/synthetic/next'); await submit();
  expect(input('历史保存位置').value).toBe('/synthetic/next');
  expect(root.querySelector('.wb-settings-locations')!.textContent).toContain('/synthetic/<img src=x>/history/runs');
  expect(root.textContent).toContain('/synthetic/<img src=x>/history/runs/live');
  expect(root.textContent).toContain('待下次启动生效：历史保存位置');
  expect(root.textContent).toContain('本次由启动参数覆盖');
  expect(root.querySelector('img')).toBeNull();
});
it('shows the configuration file only when needed to repair invalid settings', async () => {
  const server = data(); server.saved = null; server.revision = null; server.errors = [{ code: 'invalid_config', field: '$', line: 3 }];
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(ok(server))); await show(); await click('设置');
  expect(root.querySelector('[role=alert]')!.textContent).toContain('第 3 行');
  expect(root.textContent).toContain(`需要修复的配置文件：${server.configPath}`);
  expect(root.querySelector('form')).toBeNull();
});
