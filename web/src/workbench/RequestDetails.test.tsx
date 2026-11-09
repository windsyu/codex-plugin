import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { RequestDetails, ResponseUsage } from './RequestDetails';
import type { ResponseView } from './reading';
import { readSnapshot } from './reading';
import { CallInspector } from './CallInspector';
import type { ContextPresentation } from './contextSummary';

const root = document.createElement('div');
const entry = (index: number) => ({ source: { kind: 'request', clientRequestIndex: null }, captureSeq: 1, section: 'input', position: index, preview: `<svg onload="alert(1)"> literal context ${index}`, truncated: false, omitted: false });
const page = (overrides = {}) => ({ runEpoch: 'epoch', requestId: 'request', revision: 1, availability: 'captured', requestCaptured: true, responseCaptured: false, truncated: false, omitted: true, conflict: false, totalEntries: 2, entries: [entry(0)], nextCursor: 'next', captureIssues: [], ...overrides });
const response = (overrides = {}): ResponseView => ({ requestId: 'request', responseId: 'response', status: 'receiving', reportedModels: ['fixture'], ...overrides });
const success = (value: unknown) => ({ ok: true, status: 200, text: async () => JSON.stringify(value) });
const props = { epoch: 'epoch', requestId: 'request', responses: [response()], expanded: true, onToggle: () => {} };
const contextSummary: ContextPresentation = { schemaVersion: 1, contentBytes: 20, estimatedTokens: 5, tokens: 10, calibrated: true, reportedInputTokens: 10,
  coverage: { complete: true, issues: [] }, parts: [{ kind: 'files', contentBytes: 20, estimatedTokens: 5, tokens: 10, itemCount: 1 }],
  items: [{ id: 'item/opaque', kind: 'files', name: 'fixture.txt', tag: 'file', occurrences: 1, contentBytes: 20, estimatedTokens: 5, tokens: 10, references: ['/input/0'], referencesPartial: false }] };
const contextPage = (overrides = {}) => page({ contextSummary, selectedContext: 'doc/http/r1', contextSources: [
  { id: 'doc/http/r1', clientRequestIndex: null, captureSeq: 1, available: true },
  { id: 'doc/ws/r1', clientRequestIndex: 2, captureSeq: 1, available: true },
], ...overrides });
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
  it('labels only the proven WebSocket create response and preserves unmatched context', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(success(page({ entries: [
      { ...entry(0), source: { kind: 'request', clientRequestIndex: 2 } },
      { ...entry(1), source: { kind: 'request', clientRequestIndex: 3 } },
    ] }))));
    await show({ requests: [{ requestId: 'request', clientRequestIndex: 2, responseId: 'first', requestedModel: 'model', purpose: 'conversation', purposeBasis: 'codex_turn_metadata', codexThreadId: 'thread', codexTurnId: 'turn' }] });
    const summaries = [...root.querySelectorAll('.wb-context-entry summary')].map(item => item.textContent);
    expect(summaries[0]).toContain('WebSocket create 2 · 响应 first');
    expect(summaries[1]).toContain('WebSocket create 3 · 响应关联未确认');
  });
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
  it('binds category and item reads to the opaque document and retains summary on continuation pages', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(success(contextPage()))
      .mockResolvedValueOnce(success(contextPage()))
      .mockResolvedValueOnce(success(contextPage()))
      .mockResolvedValueOnce(success(page({ entries: [entry(1)], nextCursor: null })));
    vi.stubGlobal('fetch', fetch); await show();
    await act(async () => (root.querySelector('.wb-context-part') as HTMLButtonElement).click());
    await act(async () => {});
    expect(fetch.mock.calls[1][0]).toContain('document=doc%2Fhttp%2Fr1&category=files');
    expect(root.textContent).toContain('原消息可能包含其他分类');
    await act(async () => (root.querySelector('.wb-context-items button') as HTMLButtonElement).click());
    await act(async () => {});
    expect(fetch.mock.calls[2][0]).toContain('document=doc%2Fhttp%2Fr1&category=files&item=item%2Fopaque');
    await click('加载后续内容');
    expect(fetch.mock.calls[3][0]).toContain('cursor=next&document=doc%2Fhttp%2Fr1');
    expect(root.textContent).toContain('fixture.txt');
    expect(root.querySelectorAll('details')).toHaveLength(2);
  });
  it('cancels superseded filters and ignores their late response and finally handlers', async () => {
    let resolveOld!: (value: unknown) => void, resolveNew!: (value: unknown) => void;
    const fetch = vi.fn().mockResolvedValueOnce(success(contextPage()))
      .mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; }))
      .mockImplementationOnce(() => new Promise(resolve => { resolveNew = resolve; }));
    vi.stubGlobal('fetch', fetch); await show();
    await act(async () => (root.querySelector('.wb-context-part') as HTMLButtonElement).click());
    await act(async () => {});
    const oldSignal = fetch.mock.calls[1][1].signal;
    await act(async () => {
      const select = root.querySelector('select')!;
      select.value = 'doc/ws/r1'; select.dispatchEvent(new Event('change', { bubbles: true }));
    });
    expect(oldSignal.aborted).toBe(true);
    expect(fetch.mock.calls[2][0]).toContain('document=doc%2Fws%2Fr1');
    await act(async () => resolveOld(success(contextPage({ entries: [entry(8)] }))));
    expect(root.textContent).toContain('正在读取');
    expect(root.textContent).not.toContain('literal context 8');
    await act(async () => resolveNew(success(contextPage({ selectedContext: 'doc/ws/r1', entries: [entry(9)] }))));
    await act(async () => {});
    expect(root.textContent).not.toContain('正在读取');
    expect(root.textContent).toContain('literal context 9');
    expect(root.querySelector('select')!.value).toBe('doc/ws/r1');
  });
  it('retains revision-bound summary on conflict and refreshes without stale selectors', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(success(contextPage()))
      .mockResolvedValueOnce({ ok: false, status: 409 })
      .mockResolvedValueOnce(success(contextPage({ revision: 2, selectedContext: 'doc/http/r2', contextSummary: { ...contextSummary, tokens: 20 } })));
    vi.stubGlobal('fetch', fetch); await show({ historical: true, before: 40 });
    expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/history/epoch/requests/request?before=40');
    await act(async () => (root.querySelector('.wb-context-items button') as HTMLButtonElement).click());
    await act(async () => {});
    expect(root.textContent).toContain('上下文或运行已更新');
    expect(root.querySelector('.wb-context-summary')!.textContent).toContain('10 token');
    await click('刷新上下文');
    expect(fetch.mock.calls[2][0]).toBe('/workbench/v1/history/epoch/requests/request?before=40');
    expect(root.querySelector('.wb-context-summary')!.textContent).toContain('20 token');
  });
  it('keeps legacy context readable and aborts reads on collapse', async () => {
    const fetch = vi.fn().mockResolvedValueOnce(success(page({ contextSummary: null, contextSources: [], selectedContext: null })))
      .mockImplementationOnce(() => new Promise(() => {}));
    vi.stubGlobal('fetch', fetch); await show();
    expect(root.textContent).toContain('未记录分类摘要');
    expect(root.querySelectorAll('details')).toHaveLength(1);
    await click('刷新上下文');
    const signal = fetch.mock.calls[1][1].signal;
    await show({ expanded: false }); expect(signal.aborted).toBe(true);
  });
  it('allows old historical sources without a summary to fetch their original entries', async () => {
    const legacy = contextPage({ contextSummary: null, contextSources: [
      { id: 'doc/http/r1', clientRequestIndex: null, captureSeq: 1, available: false },
      { id: 'doc/ws/r1', clientRequestIndex: 2, captureSeq: 1, available: false },
    ] });
    const fetch = vi.fn().mockResolvedValueOnce(success(legacy)).mockResolvedValueOnce(success({ ...legacy, selectedContext: 'doc/ws/r1', entries: [entry(4)] }));
    vi.stubGlobal('fetch', fetch); await show({ historical: true });
    expect(root.textContent).toContain('此文档未记录分类摘要');
    expect([...root.querySelectorAll('option')].every(option => !option.disabled)).toBe(true);
    await act(async () => {
      const select = root.querySelector('select')!;
      select.value = 'doc/ws/r1'; select.dispatchEvent(new Event('change', { bubbles: true }));
    });
    await act(async () => {});
    expect(fetch.mock.calls[1][0]).toContain('document=doc%2Fws%2Fr1');
    expect(root.textContent).toContain('literal context 4');
    expect(root.textContent).toContain('未记录分类摘要');
  });
  it('prompts for changed request metadata without reloading or losing reading focus', async () => {
    const fetch = vi.fn().mockResolvedValue(success(contextPage())); vi.stubGlobal('fetch', fetch);
    await show({ metadataVersion: 'http-summary' });
    const details = root.querySelector('details')!; details.open = true;
    const summary = details.querySelector('summary')!; summary.focus();
    await show({ metadataVersion: 'http-and-ws-summary' });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(root.textContent).toContain('请求上下文，可刷新上下文');
    expect(root.querySelector('details')).toBe(details); expect(details.open).toBe(true);
    expect(document.activeElement).toBe(summary);
    await click('刷新上下文');
    expect(root.textContent).not.toContain('请求上下文，可刷新上下文');
  });
  it('passes new WebSocket metadata through the inspector and memoizes it across token updates', async () => {
    const fetch = vi.fn().mockResolvedValue(success(contextPage())); vi.stubGlobal('fetch', fetch);
    const readSummary = vi.fn(() => null);
    const metadata = { requestId: 'request', clientRequestIndex: null, requestedModel: 'fixture', codexThreadId: null, codexTurnId: null,
      purpose: 'unknown', purposeBasis: 'missing_metadata', get contextSummary() { return readSummary(); } };
    const reading = readSnapshot({ runEpoch: 'epoch', schemaVersion: 2, viewSeq: 1, requests: [metadata], responses: [], items: [], diagnostics: [], toolContexts: [] }, 'epoch');
    const showInspector = async (view = reading) => {
      await act(async () => render(<CallInspector reading={view} epoch="epoch" requestId="request" onSelect={() => {}} onClose={() => {}} />, root));
      await act(async () => {});
    };
    await showInspector();
    expect(readSummary).toHaveBeenCalledTimes(1);
    await showInspector({ ...reading, viewSeq: 2 });
    expect(readSummary).toHaveBeenCalledTimes(1);
    expect(root.textContent).not.toContain('请求上下文，可刷新上下文');
    await showInspector({ ...reading, viewSeq: 3, requests: [...reading.requests, { ...reading.requests[0], clientRequestIndex: 1 }] });
    expect(root.textContent).toContain('请求上下文，可刷新上下文');
    expect(fetch).toHaveBeenCalledTimes(1);
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
