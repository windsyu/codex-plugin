import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RequestDetails, ResponseUsage } from './RequestDetails';
import type { ResponseView } from './reading';

const root = document.createElement('div');
const entry = (index: number) => ({ source: { kind: 'request', clientRequestIndex: null }, captureSeq: 1, section: 'input', position: index, preview: `<svg onload="alert(1)"> literal context ${index}`, truncated: false, omitted: false });
const page = (overrides = {}) => ({ runEpoch: 'epoch', requestId: 'request', revision: 1, availability: 'captured', requestCaptured: true, responseCaptured: false, truncated: false, omitted: true, conflict: false, totalEntries: 2, entries: [entry(0)], nextCursor: 'next', captureIssues: [], ...overrides });
const response = (overrides = {}): ResponseView => ({ requestId: 'request', responseId: 'response', status: 'receiving', reportedModels: ['fixture'], ...overrides });
const success = (value: unknown) => ({ ok: true, status: 200, text: async () => JSON.stringify(value) });
const props = { epoch: 'epoch', requestId: 'request', responses: [response()], expanded: true, onToggle: () => {} };
beforeEach(() => { document.body.append(root); });
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });
const click = async (name: string) => {
  const button = [...root.querySelectorAll('button')].find(button => button.textContent === name)!;
  await act(async () => button.click());
  await act(async () => {});
};

async function show(overrides: Partial<Parameters<typeof RequestDetails>[0]> = {}) {
  await act(async () => render(<RequestDetails {...props} {...overrides} />, root));
  await act(async () => {});
}

describe('on-demand request reading', () => {
  it('fetches only on expansion, escapes context and preserves details and focus on streamed updates', async () => {
    const fetch = vi.fn().mockResolvedValue(success(page())); vi.stubGlobal('fetch', fetch);
    await show({ expanded: false });
    expect(fetch).not.toHaveBeenCalled();
    await show();
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/requests/request?epoch=epoch');
    expect(root.querySelector('svg,[data-role]')).toBeNull();
    expect(root.textContent).toContain('<svg onload="alert(1)">');
    expect(root.textContent).toContain('按策略省略');
    const details = root.querySelector('details')!; details.open = true;
    const summary = details.querySelector('summary')!; summary.focus();
    await show({ responses: [response({ status: 'completed' })] });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(root.querySelector('details')).toBe(details); expect(details.open).toBe(true);
    expect(document.activeElement).toBe(summary);
    expect(root.textContent).toContain('已收到新的响应状态');
  });
  it('appends pages but keeps the old view on a stale cursor, then refreshes explicitly', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(success(page())).mockResolvedValueOnce(success(page({ entries: [entry(1)], nextCursor: 'later' }))).mockResolvedValueOnce({ ok: false, status: 409 }).mockResolvedValueOnce(success(page({ revision: 2, totalEntries: 1, nextCursor: null, entries: [entry(2)], conflict: true, captureIssues: ['observation_gap'] })));
    vi.stubGlobal('fetch', fetch);
    await show();
    const first = root.querySelector('details')!; first.open = true;
    await click('加载后续内容');
    expect(root.querySelectorAll('details')).toHaveLength(2);
    expect(root.querySelector('details')).toBe(first); expect(first.open).toBe(true);
    await click('加载后续内容');
    expect(root.textContent).toContain('上下文或运行已更新');
    expect(root.querySelectorAll('details')).toHaveLength(2);
    await click('刷新上下文');
    expect(root.querySelectorAll('details')).toHaveLength(1);
    expect(root.textContent).toContain('同一来源出现不同内容');
    expect(root.textContent).toContain('观察记录存在缺口');
    expect(root.textContent).not.toContain('上下文或运行已更新');
  });
  it('never applies an old fetch after selecting another request and rejects mismatched run data', async () => {
    let resolve!: (value: unknown) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise(done => { resolve = done; })).mockResolvedValueOnce(success(page({ runEpoch: 'other-epoch' })));
    vi.stubGlobal('fetch', fetch);
    await show();
    const signal = fetch.mock.calls[0][1].signal;
    await show({ requestId: 'another-request' });
    expect(signal.aborted).toBe(true);
    await act(async () => resolve(success(page())));
    expect(root.querySelectorAll('details')).toHaveLength(0);
    expect(root.textContent).toContain('上下文或运行已更新');
  });
  it.each(['pending', 'unavailable', 'evicted'])('shows %s without inventing an empty successful context', async availability => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(availability === 'evicted' ? { ok: false, status: 410 } : success(page({ availability, requestCaptured: false, entries: [], totalEntries: 0, nextCursor: null }))));
    await show();
    expect(root.textContent).toContain(availability === 'pending' ? '尚未捕获完整请求' : availability === 'unavailable' ? '当前没有可用的上下文' : '已移出内存缓存');
    expect(root.querySelectorAll('details')).toHaveLength(0);
  });
  it('bounds accumulated pages and lets the reader continue by replacing the current group', async () => {
    let offset = 0;
    const fetch = vi.fn().mockImplementation(async () => success(page({ totalEntries: 400, entries: Array.from({ length: 16 }, (_, n) => entry(offset++)), nextCursor: `after-${offset}` })));
    vi.stubGlobal('fetch', fetch);
    await show();
    for (let n = 0; n < 16; n++) await click('加载后续内容');
    expect(root.querySelectorAll('details')).toHaveLength(256);
    expect(root.textContent).toContain('当前内容已达阅读上限');
    await click('继续阅读后续页');
    expect(root.querySelectorAll('details')).toHaveLength(16);
  });
});

it('labels per-response usage, keeps zero distinct from missing data and exposes invalid/conflicting reports', () => {
  render(<ResponseUsage response={response({ usage: { inputTokens: 10, outputTokens: 0, totalTokens: 10, cachedInputTokens: 0, cacheWriteTokens: null, reasoningTokens: null, invalid: true }, usageConflict: true, observedDurationMs: 120 })} />, root);
  expect(root.querySelectorAll('dd')[2].textContent).toBe('0');
  expect(root.querySelectorAll('dd')[3].textContent).toBe('未提供');
  expect(root.textContent).toContain('0.12 秒');
  expect(root.textContent).toContain('计数不一致');
  expect(root.textContent).toContain('收到冲突用量');
  render(<ResponseUsage response={response({ responseId: 'response-without-usage' })} />, root);
  expect(root.textContent).toContain('缺失值不计为零');
  expect(root.textContent).not.toContain('0.12 秒');
});
