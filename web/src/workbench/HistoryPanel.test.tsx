import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { HistoryPanel } from './HistoryPanel';
import { RecorderDetails, saveLabel } from './RecorderStatus';
import { applyRecorderStatus, readSnapshot, type RecorderStatus } from './reading';

const root = document.createElement('div');
beforeEach(() => document.body.append(root));
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); vi.useRealTimers(); });
const success = (value: unknown) => ({ ok: true, status: 200, json: async () => value });
const run = { runEpoch: 'old', projectName: '<svg onload=alert(1)>', startedAt: '2026-09-20T00:00:00Z', state: 'unclean', savedThroughViewSeq: 2, persistedThroughViewSeq: 1, gapCount: 1, historyCoverage: 'partial' };
const snapshot = { runEpoch: 'old', schemaVersion: 2, viewSeq: 2, items: [], requests: [], responses: [], diagnostics: [], toolContexts: [], nativeCommands: [], nativeFileChanges: [], userCapture: { enabled: true, diagnostics: [] }, capture: 'partial' };
const list = { currentRunEpoch: 'current', runs: [run], nextCursor: null, diagnostics: [], indexState: 'ready' };
const saved = { currentRunEpoch: 'current', run, snapshot, uncertainTail: true, gaps: [{ afterViewSeq: 1, throughViewSeq: 2, reason: 'queue_full' }], issues: [{ segment: 'segment', byteOffset: 15, code: 'partial_line' }], detailsPartial: false };
// jsdom has no native modal implementation; Chrome covers focus and Escape.
HTMLDialogElement.prototype.showModal = function () { this.open = true; };
HTMLDialogElement.prototype.close = function () { this.open = false; };
async function click(text: string) { await act(async () => [...root.querySelectorAll('button')].find(b => b.textContent!.includes(text))!.click()); await act(async () => {}); }
it('loads history on demand, escapes labels and keeps history separate from native input', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(success(list)).mockResolvedValueOnce(success(saved)); vi.stubGlobal('fetch', fetch);
  const onReturn = vi.fn();
  await act(async () => render(<HistoryPanel epoch="current" active={false} onReturn={onReturn} />, root));
  expect(fetch).not.toHaveBeenCalled();
  await act(async () => render(<HistoryPanel epoch="current" active onReturn={onReturn} />, root)); await act(async () => {});
  expect(root.querySelector('svg')).toBeNull();
  await click('<svg');
  expect(root.textContent).toContain('右侧终端始终属于当前运行');
  expect(root.textContent).toContain('曾显示但未保存的尾部可能已经丢失');
  expect(root.textContent).toContain('保存有 1 处缺口');
  expect(root.textContent).toContain('日志尾部不完整');
  expect(fetch.mock.calls.map(c => c[0])).toEqual(['/workbench/v1/history', '/workbench/v1/history/old']);
  expect(fetch.mock.calls.every(c => !c[1].method || c[1].method === 'GET')).toBe(true);
  await click('返回实时阅读'); expect(onReturn).toHaveBeenCalledOnce();
});
it('keeps the prior selection on unavailable or mismatched history and permits a retry', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(success(list)).mockResolvedValueOnce({ ok: false, status: 503 }).mockResolvedValueOnce(success({ ...saved, currentRunEpoch: 'other' })); vi.stubGlobal('fetch', fetch);
  await act(async () => render(<HistoryPanel epoch="current" active onReturn={() => {}} />, root)); await act(async () => {});
  await click('<svg'); expect(root.textContent).toContain('历史暂不可读取');
  await click('<svg'); expect(root.textContent).toContain('当前运行已改变');
  expect(root.textContent).not.toContain('曾显示但未保存');
});
it('retains historical reading on transient status errors and clears it plus the inspector after deletion', async () => {
  let checks=0;
  const fetch=vi.fn(async (url:string)=>url.endsWith('/status')?{ok:false,status:++checks===1?503:410}:success(url==='/workbench/v1/history'?list:url==='/workbench/v1/history/old'?saved:{currentRunEpoch:'current',result:{scheduled:true}}));
  vi.stubGlobal('fetch',fetch);const changed=vi.fn(), onReturn=()=>{};
  await act(async()=>{render(<HistoryPanel epoch="current" active onReturn={onReturn} onSelectionChange={changed}/>,root);});
  await act(async()=>{});
  await click('<svg');
  vi.useFakeTimers();
  await act(async()=>{render(<HistoryPanel epoch="current" active managementAvailable onReturn={onReturn} onSelectionChange={changed}/>,root);});
  await act(async()=>{await vi.advanceTimersByTimeAsync(3000);});
  expect(root.querySelector('.wb-saved-reading')).not.toBeNull();
  await act(async()=>{await vi.advanceTimersByTimeAsync(3000);});
  expect(root.querySelector('.wb-saved-reading')).toBeNull();expect(root.textContent).toContain('此历史已删除或不存在');expect(changed).toHaveBeenCalledTimes(2);
});
it('shows unknown historical items as safe diagnostics and distinguishes capture gaps from saving', async () => {
  const unknown = { itemKey: 'future', revision: 1, orderIndex: 1, kind: 'future', payload: '<script>secret-unknown-payload</script>' };
  const fetch = vi.fn().mockResolvedValueOnce(success(list)).mockResolvedValueOnce(success({ ...saved, snapshot: { ...snapshot, items: [unknown] } }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(<HistoryPanel epoch="current" active onReturn={() => {}} />, root)); await act(async () => {});
  await click('<svg');
  expect(root.textContent).toContain('未识别的内容类型');
  expect(root.textContent).toContain('已保存不等于完整捕获');
  expect(root.textContent).not.toContain('secret-unknown-payload');
  expect(root.querySelector('script')).toBeNull();
});
it('recorder control messages do not advance reading cursors and never hide a saved-prefix gap', () => {
  const status: RecorderStatus = { runEpoch: 'old', state: 'saved', savedThroughViewSeq: 2, persistedThroughViewSeq: 1, observedViewSeq: 2, historyCoverage: 'partial', gapCount: 1, error: null };
  const before = readSnapshot(snapshot, 'old'); const after = applyRecorderStatus(before, status, 'old');
  expect(after.viewSeq).toBe(before.viewSeq); expect(after.items).toBe(before.items);
  expect(() => applyRecorderStatus(before, status, 'other')).toThrow();
  expect(() => applyRecorderStatus(before, { ...status, persistedThroughViewSeq: 3 }, 'old')).toThrow();
  expect(saveLabel(status, 3)).toBe('正在保存'); expect(saveLabel(status, 2)).toBe('已保存 · 曾有缺口');
  render(<RecorderDetails status={{ ...status, state: 'degraded', error: 'sync_failed' }} viewSeq={3} />, root);
  expect(root.textContent).toContain('磁盘同步失败'); expect(root.textContent).toContain('终端和实时阅读仍可使用');
});
it('loads additional runs and earlier saved windows, keeps the selection on errors, and returns to the latest saved window', async () => {
  const fetch = vi.fn()
    .mockResolvedValueOnce(success({ ...list, nextCursor: 'generation.20' }))
    .mockResolvedValueOnce(success({ ...list, runs: [{ ...run, runEpoch: 'older', projectName: 'Older run' }] }))
    .mockResolvedValueOnce(success({ ...saved, previousBefore: 1, before: null }))
    .mockResolvedValueOnce(success({ ...saved, previousBefore: null, before: 1 }))
    .mockResolvedValueOnce({ ok: false, status: 503 })
    .mockResolvedValueOnce(success({ ...saved, previousBefore: 1, before: null }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(<HistoryPanel epoch="current" active onReturn={() => {}} />, root)); await act(async () => {});
  await click('加载更多运行');
  expect(root.querySelectorAll('.wb-history-list button')).toHaveLength(2);
  await click('<svg');
  await click('查看更早的记录');
  expect(root.textContent).not.toContain('查看更早的记录');
  expect(root.textContent).toContain('最近保存内容');
  await click('最近保存内容');
  expect(root.textContent).toContain('历史暂不可读取');
  expect(root.querySelector('h2')?.textContent).toContain('old');
  await click('最近保存内容');
  expect(root.textContent).not.toContain('最近保存内容');
  expect(fetch.mock.calls.map(c => c[0])).toEqual(['/workbench/v1/history', '/workbench/v1/history?cursor=generation.20', '/workbench/v1/history/old', '/workbench/v1/history/old?before=1', '/workbench/v1/history/old', '/workbench/v1/history/old']);
});
function managementFixture() {
  const records = [{ ...run, runEpoch: 'current', state: 'active' }, { ...run, runEpoch: 'ended', state: 'ended' }, { ...run, runEpoch: 'unsafe', state: 'unclean', storage: { manualEligible: false, reason: 'unsafe_or_unreadable_entry' } }];
  const fetch = vi.fn(async (url: string, options?: RequestInit) => {
    if (url === '/workbench/v1/history') return success({ ...list, runs: records, nextCursor: 'page2' });
    if (url.endsWith('?cursor=page2')) return success({ ...list, runs: [{ ...run, runEpoch: 'older', state: 'ended' }], nextCursor: null });
    if (url.endsWith('/cleanup/preview')) return success({ currentRunEpoch: 'current', result: { previewId: 'preview' } });
    if (url.endsWith('/cleanup/previews/preview')) return success({ currentRunEpoch: 'current', result: { previewId: 'preview', status: 'ready', executable: false, mode: 'manual', items: [], scanComplete: true, skippedCounts: {} } });
    return success({ currentRunEpoch: 'current', result: { scheduled: true } });
  });
  vi.stubGlobal('fetch', fetch);
  return fetch;
}
it('starts with reading and per-record delete, then confirms exactly one record without deleting on cancel', async () => {
  const fetch = managementFixture();
  await act(async () => render(<HistoryPanel epoch="current" active managementAvailable onReturn={() => {}} />, root));
  await act(async () => {});
  expect(root.querySelectorAll('input[type=checkbox]')).toHaveLength(0);
  expect(root.querySelectorAll('.wb-history-delete')).toHaveLength(1);
  expect(root.textContent).toContain('正在运行，暂不能删除');
  expect(fetch.mock.calls.some(([url]) => url.endsWith('/cleanup/jobs'))).toBe(false);
  await act(async () => (root.querySelector('.wb-history-delete') as HTMLButtonElement).click());
  await act(async () => {});
  expect(root.querySelector('dialog[open]')).not.toBeNull();
  const preview = fetch.mock.calls.find(([url]) => url.endsWith('/cleanup/preview'))!;
  expect(JSON.parse(preview[1]!.body as string).runEpochs).toEqual(['ended']);
  await click('取消');
  expect(root.querySelector('dialog')).toBeNull();
  expect(fetch.mock.calls.some(([url]) => url.endsWith('/cleanup/jobs'))).toBe(false);
});
it('selects only deletable loaded records across pages and exits selection without a deletion', async () => {
  const fetch = managementFixture();
  await act(async () => render(<HistoryPanel epoch="current" active managementAvailable onReturn={() => {}} />, root));
  await act(async () => {});
  await click('加载更多运行'); await click('批量删除');
  const checkboxes = [...root.querySelectorAll<HTMLInputElement>('input[type=checkbox]')];
  expect(checkboxes.filter(input => input.disabled)).toHaveLength(2);
  await act(async () => checkboxes[0].click());
  expect(root.textContent).toContain('已选 2 条');
  await click('删除所选');
  const preview = fetch.mock.calls.find(([url]) => url.endsWith('/cleanup/preview'))!;
  expect(JSON.parse(preview[1]!.body as string).runEpochs).toEqual(['ended', 'older']);
  await click('取消'); await click('退出选择');
  expect(root.querySelectorAll('input[type=checkbox]')).toHaveLength(0);
  expect(fetch.mock.calls.some(([url]) => url.endsWith('/cleanup/jobs'))).toBe(false);
});
it('explains disabled deletion and opens existing settings without submitting a deletion', async () => {
  const fetch = managementFixture(), settings = vi.fn();
  await act(async () => render(<HistoryPanel epoch="current" active managementAvailable onReturn={() => {}} onSettings={settings} />, root));
  await act(async () => {});
  await act(async () => (root.querySelector('.wb-history-delete') as HTMLButtonElement).click());
  await act(async () => {});
  await vi.waitFor(() => expect(root.textContent).toContain('历史删除尚未开启'));
  await click('前往清理设置');
  expect(settings).toHaveBeenCalledOnce(); expect(root.querySelector('dialog')).toBeNull();
  expect(fetch.mock.calls.some(([url]) => url.endsWith('/cleanup/jobs'))).toBe(false);
});
