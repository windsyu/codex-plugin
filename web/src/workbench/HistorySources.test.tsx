import { render } from 'preact';
import { useState } from 'preact/hooks';
import { act } from 'preact/test-utils';
import { afterEach, expect, it, vi } from 'vitest';
import { HistorySources, LibraryConfig, libraryDefaults } from './HistorySources';
const root = document.createElement('div');
let value: LibraryConfig;
function Harness() { const [draft, setDraft] = useState(libraryDefaults()); value = draft; return <HistorySources value={draft} onChange={setDraft} />; }
const button = (name: string) => [...root.querySelectorAll('button')].find(b => b.textContent === name)!;
async function click(name: string) { await act(async () => button(name).click()); }
async function input(name: string, text: string) { await act(async () => { const field = root.querySelector(`[aria-label="${name}"]`) as HTMLInputElement; field.value = text; field.dispatchEvent(new Event('input', { bubbles: true })); }); }
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
async function show() { document.body.append(root); await act(async () => render(<Harness />, root)); }
it('adds and removes sources in the draft without registering or deleting any history', async () => {
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch); await show(); await click('接入旧历史');
  await input('历史文件位置', '/synthetic/<script>old.sqlite'); await click('加入待保存来源');
  expect(value.sources).toHaveLength(1); expect(root.textContent).toContain('<script>old.sqlite'); expect(root.querySelector('script')).toBeNull(); expect(fetch).not.toHaveBeenCalled();
  await click('移除来源'); expect(value.sources).toHaveLength(0); expect(fetch).not.toHaveBeenCalled();
});
it('requires explicit preview acceptance before adding the selected Observer paths', async () => {
  const source = { kind: 'observer', id: 'old', database: '/synthetic/old.sqlite', blobDirectory: '/synthetic/blobs' };
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ source }) }); vi.stubGlobal('fetch', fetch);
  await show(); await click('接入旧历史'); await input('旧版配置文件', '/synthetic/observer.toml'); await click('读取位置');
  expect(value.sources).toHaveLength(0); expect(fetch).toHaveBeenCalledTimes(1); expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/library/preview');
  await click('确认加入待保存来源'); expect(value.sources).toEqual([source]); expect(fetch).toHaveBeenCalledTimes(1);
});
it('rejects failed previews without changing configured sources', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue({ ok: false, json: async () => ({ error: { code: 'invalid' } }) }));
  await show(); await click('接入旧历史'); await input('旧版配置文件', '/missing.toml'); await click('读取位置');
  expect(root.querySelector('[role="alert"]')?.textContent).toContain('无法读取'); expect(value.sources).toHaveLength(0);
});
