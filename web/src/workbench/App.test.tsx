import { render } from 'preact';
import { act } from 'preact/test-utils';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { App } from './App';
import { readSnapshot, type ReadingView, type RequestView } from './reading';
import type { ModelItem } from './viewItems';

let reading: ReadingView;
let reportExit: ((exit: { code: number; signal: string | null }) => void) | undefined;
vi.mock('./reading', async original => ({ ...await original<typeof import('./reading')>(), useReading: () => reading }));
vi.mock('./TerminalPanel', () => ({ TerminalPanel: ({ onExit }: { onExit?: typeof reportExit }) => {
  reportExit = onExit;
  return <textarea aria-label="Native terminal" />;
} }));
const run = { runEpoch: 'live', terminalAvailable: true, projectName: 'Synthetic project', processId: 42, historyAvailable: true };
const metadata = (id: string, purpose: RequestView['purpose'] = 'conversation'): RequestView => ({ requestId: id, clientRequestIndex: null, requestedModel: 'model', codexThreadId: 'thread', codexTurnId: 'turn', purpose, purposeBasis: 'codex_turn_metadata' });
const model = (id: string): ModelItem => ({ kind: 'message', itemKey: id, revision: 1, orderIndex: 1, completeness: 'observed', truncated: false, streamState: 'receiving', content: [{ contentKey: 'text', text: `Body ${id}` }], author: { role: 'assistant', requestedModel: 'model', reportedModels: [] }, evidence: [{ origin: 'model', key: { requestId: id, responseId: id, wireItemId: 'message', contentIndex: 0 }, captureSeq: 1 }] });
const snapshot = (epoch: string) => ({ runEpoch: epoch, schemaVersion: 2, viewSeq: 1, items: [model('chat'), model('aux')], requests: [metadata('chat'), metadata('aux', 'auxiliary')], responses: [{ requestId: 'chat', responseId: 'chat', status: 'receiving', reportedModels: [] }], diagnostics: [], toolContexts: [], nativeCommands: [], nativeFileChanges: [], userCapture: { enabled: true, diagnostics: [] }, capture: 'ok' });
const detail = (epoch: string, id: string) => ({ runEpoch: epoch, requestId: id, revision: 1, availability: 'captured', requestCaptured: true, responseCaptured: true, truncated: false, omitted: false, conflict: false, totalEntries: 1, entries: [{ source: { kind: 'request', clientRequestIndex: null }, captureSeq: 1, section: 'input', position: 0, preview: '<svg onload="alert(1)"> literal context', truncated: false, omitted: false }], nextCursor: null, captureIssues: [] });
const root = document.createElement('div');
const success = (value: unknown) => ({ ok: true, status: 200, json: async () => value, text: async () => JSON.stringify(value) });
const button = (name: string, base: ParentNode = root) => [...base.querySelectorAll('button')].find(b => b.getAttribute('aria-label') === name || b.textContent!.trim() === name)!;
async function click(target: string | HTMLElement) { await act(async () => { const b = typeof target === 'string' ? button(target) : target; b.focus(); b.click(); }); await act(async () => {}); }
async function show() { await act(async () => render(<App run={run} />, root)); await act(async () => {}); }
it('keeps Git beside conversation and the same terminal through file reading and return', async () => {
  const fetch = vi.fn().mockImplementation(async (url: string) => success(url.includes('git/status')
    ? { branch: 'work', entries: [{ path: 'code.ts', index: ' ', working: 'M' }], readAt: '2026-09-20T12:00:00Z', elapsedMs: 1 }
    : { path: 'code.ts', patch: '@@ -1 +1 @@\n-old\n+new', revision: 'one', readAt: '2026-09-20T12:00:00Z', elapsedMs: 1 }));
  vi.stubGlobal('fetch', fetch);
  await act(async () => render(<App run={{ ...run, workspaceRoot: '/synthetic' }} />, root));
  const terminal = root.querySelector('textarea')!; terminal.value = 'unchanged draft';
  const chat = root.querySelector('.wb-message-list') as HTMLElement;
  await click('Git'); expect(chat.hidden).toBe(false); expect(root.querySelector('.wb-git-row')).not.toBeNull();
  await click(root.querySelector('.wb-git-row button') as HTMLElement); expect(chat.hidden).toBe(true);
  expect(root.querySelector('.wb-git-row button[aria-current="true"]')?.getAttribute('title')).toBe('code.ts');
  await vi.waitFor(() => expect(root.querySelector('.wb-file-reading')?.textContent).toContain('+new'));
  await click('关闭文件阅读'); expect(chat.hidden).toBe(false);
  expect(root.querySelector('.wb-git-row button[aria-current="true"]')).toBeNull();
  expect(root.querySelector('textarea')).toBe(terminal);expect(terminal.value).toBe('unchanged draft');
});
// jsdom has no text layout; real selection geometry is covered by Chrome.
beforeEach(() => {
  reportExit = undefined;
  HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  HTMLDialogElement.prototype.close = function () { this.open = false; };
  Object.defineProperty(Range.prototype, 'getClientRects', { configurable: true, value: () => [] });
  Object.defineProperty(Range.prototype, 'getBoundingClientRect', { configurable: true, value: () => new DOMRect() });
  document.body.append(root); reading = readSnapshot(snapshot('live'), 'live');
});
afterEach(() => { render(null, root); root.remove(); vi.unstubAllGlobals(); });

it('announces CLI exit centrally even with the terminal hidden and keeps records after dismissal', async () => {
  await act(async () => render(<App run={run} applicationHome />, root));
  const terminal = root.querySelector('textarea');
  const messages = root.querySelector('.wb-message-list');
  await click('终端');
  expect(root.querySelector('dialog[open]')).toBeNull();
  await act(async () => reportExit?.({ code: 1, signal: 'Terminated: 15' }));
  const dialog = root.querySelector('dialog[open]')!;
  expect(dialog?.textContent).toContain('本次运行已结束');
  expect(dialog.textContent).toContain(run.projectName);
  expect(dialog.querySelector('a')?.getAttribute('href')).toBe('/');
  expect(root.querySelector('.wb-header-actions')?.textContent).toContain('已结束');
  await click('继续查看记录');
  expect(root.querySelector('dialog[open]')).toBeNull();
  expect(document.activeElement).toBe(button('终端'));
  expect(root.querySelector('textarea')).toBe(terminal);
  expect(root.querySelector('.wb-message-list')).toBe(messages);
  expect(messages?.textContent).toContain('Body chat');
  await act(async () => reportExit?.({ code: 1, signal: 'Terminated: 15' }));
  expect(root.querySelector('dialog[open]')).toBeNull();
  await click('查看运行结束提示');
  expect(root.querySelector('dialog[open]')).not.toBeNull();
  await act(async () => { root.querySelector('dialog')!.dispatchEvent(new Event('cancel', { cancelable: true })); });
  expect(root.querySelector('dialog[open]')).toBeNull();
});

it('keeps a reading disconnect distinct from an exited CLI and updates the empty state only on exit', async () => {
  reading = { ...reading, connected: false, items: [], requests: [] };
  await show();
  expect(root.querySelector('dialog[open]')).toBeNull();
  expect(root.querySelector('.wb-empty')?.textContent).toContain('等待对话回复');
  await act(async () => reportExit?.({ code: 0, signal: null }));
  expect(root.querySelector('dialog[open]')).not.toBeNull();
  await click('继续查看记录');
  expect(root.querySelector('.wb-empty')?.textContent).toContain('本次运行已结束');
  expect(root.textContent).not.toContain('在右侧终端直接输入');
  await click('用量与状态');
  expect(button('本次运行已结束').disabled).toBe(true);
});

it('keeps device exit notices scoped to the current workbench and escapes exit details', async () => {
  const unsafe = '<img src=x onerror="alert(1)">';
  await act(async () => render(<App run={{ ...run, projectName: unsafe }} />, root));
  await act(async () => reportExit?.({ code: 2, signal: unsafe }));
  const dialog = root.querySelector('dialog[open]')!;
  expect(dialog?.textContent).toContain(unsafe);
  expect(dialog.querySelector('img')).toBeNull();
  expect(dialog.querySelector('a')).toBeNull();
  await click('关闭运行结束提示');
  expect(root.querySelector('dialog[open]')).toBeNull();
});

it('opens reply details on demand without duplicating chat or disturbing streaming focus and native input', async () => {
  const fetch = vi.fn().mockResolvedValue(success(detail('live', 'chat'))); vi.stubGlobal('fetch', fetch);
  await show();
  expect(fetch).not.toHaveBeenCalled();
  expect(root.querySelectorAll('[data-role="assistant"]')).toHaveLength(1);
  expect(button('网络')).toBeUndefined(); expect(button('模型请求')).toBeUndefined();
  const native = root.querySelector('textarea')!; native.value = 'draft';
  const trigger = button('调用详情'); await click(trigger);
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/requests/chat?epoch=live');
  const context = root.querySelector('.wb-context-entry') as HTMLDetailsElement;
  context.open = true; context.querySelector('summary')!.focus();
  reading = { ...reading, viewSeq: 2, items: [model('chat'), model('aux')], responses: [{ ...reading.responses[0], status: 'completed' }] };
  await show();
  expect(root.querySelector('.wb-context-entry')).toBe(context); expect(context.open).toBe(true);
  expect(document.activeElement).toBe(context.querySelector('summary'));
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(root.querySelectorAll('[data-role="assistant"]')).toHaveLength(1);
  expect(context.querySelector('svg')).toBeNull();
  expect(root.querySelector('textarea')).toBe(native); expect(native.value).toBe('draft');
  await act(async () => { context.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })); });
  expect(root.querySelector('.wb-call-inspector')).toBeNull(); expect(document.activeElement).toBe(trigger);
});

it('keeps auxiliary, unknown and context-only records reachable from usage without fetching until selected', async () => {
  reading.requests.push(metadata('unknown', 'unknown'));
  reading.diagnostics.push({ requestId: 'diagnostic', captureSeq: 1, code: 'observation_gap' });
  const fetch = vi.fn().mockResolvedValue(success(detail('live', 'aux'))); vi.stubGlobal('fetch', fetch);
  await show(); await click('用量与状态'); await click(root.querySelector('.wb-view-calls') as HTMLElement);
  expect(root.querySelectorAll('.wb-call-row')).toHaveLength(4);
  expect(fetch).not.toHaveBeenCalled();
  expect(root.querySelector('.wb-call-inspector')!.textContent).not.toContain('Body');
  expect(root.querySelector('.wb-call-inspector')!.textContent).toContain('本次运行');
  await click(root.querySelector('.wb-call-row[data-request-id="aux"]') as HTMLElement);
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/requests/aux?epoch=live');
  await click('← 全部调用'); expect(root.querySelectorAll('.wb-context-entry')).toHaveLength(0);
  await click('关闭调用面板'); expect(document.activeElement).toBe(button('用量与状态'));
});

it('offers call details for a tool-only response', async () => {
  const key = { requestId: 'chat', responseId: 'tool-only', wireItemId: 'tool', contentIndex: 0 };
  reading.items = [{ kind: 'tool_call', itemKey: 'tool', key, revision: 1, orderIndex: 1, completeness: 'observed', evidence: [{ origin: 'model', key, captureSeq: 1 }], captureSeq: 1, truncated: false, toolKind: 'function', callId: 'call', name: 'exec', namespace: null, category: 'command', arguments: '{}', argumentsState: 'generated', command: null, execution: 'unobserved', result: null, proposedPatch: null, identityConflict: false, resultConflict: false }];
  const fetch = vi.fn().mockResolvedValue(success(detail('live', 'chat'))); vi.stubGlobal('fetch', fetch);
  await show();
  expect(root.querySelector('[data-role="assistant"]')).toBeNull();
  await click('调用详情');
  expect(fetch.mock.calls[0][0]).toBe('/workbench/v1/requests/chat?epoch=live');
  expect(root.querySelectorAll('.wb-tool-card')).toHaveLength(1);
});

it('binds history inspection to its saved epoch and window while the footer still opens current calls', async () => {
  const old = { runEpoch: 'old', projectName: 'Past project', startedAt: '2026-09-20T00:00:00Z', state: 'ended', savedThroughViewSeq: 1, persistedThroughViewSeq: 1, gapCount: 0, historyCoverage: 'complete_for_observed_scope' };
  const fetch = vi.fn().mockImplementation(async (url: string) => {
    if (url === '/workbench/v1/history') return success({ currentRunEpoch: 'live', runs: [old], nextCursor: null, diagnostics: [], indexState: 'ready' });
    if (url === '/workbench/v1/history/old') return success({ currentRunEpoch: 'live', run: old, snapshot: snapshot('old'), uncertainTail: false, gaps: [], issues: [], detailsPartial: false, before: 10 });
    return success(detail(url.includes('/history/') ? 'old' : 'live', 'chat'));
  });
  vi.stubGlobal('fetch', fetch); await show();
  const native = root.querySelector('textarea');
  await click('历史记录'); await click(root.querySelector('.wb-history-list button') as HTMLElement);
  await click(button('调用详情', root.querySelector('.wb-history')!));
  expect(fetch.mock.calls.at(-1)![0]).toBe('/workbench/v1/history/old/requests/chat?before=10');
  expect(root.querySelector('.wb-call-inspector')!.textContent).toContain('历史运行 · old · 较早窗口');
  expect(root.querySelector('.wb-call-inspector')!.textContent).toContain('保存时：尚未观察到响应结束');
  await click('关闭调用面板'); await click('查看此历史的调用记录');
  await click(root.querySelector('.wb-call-row[data-request-id="chat"]') as HTMLElement);
  expect(fetch.mock.calls.at(-1)![0]).toContain('/history/old/requests/chat?before=10');
  await click('用量与状态'); await click(root.querySelector('.wb-view-calls') as HTMLElement);
  expect(root.querySelector('.wb-call-inspector')!.textContent).toContain('本次运行');
  await click(root.querySelector('.wb-call-row[data-request-id="chat"]') as HTMLElement);
  expect(fetch.mock.calls.at(-1)![0]).toBe('/workbench/v1/requests/chat?epoch=live');
  expect(root.querySelector('textarea')).toBe(native);
  expect(fetch.mock.calls.every(c => !c[1].method || c[1].method === 'GET')).toBe(true);
});

it('lists WebSocket response metrics separately, preserves zero and flags conflicting usage', async () => {
  reading.requests = [{ ...metadata('ws'), clientRequestIndex: 1 }, { ...metadata('ws'), clientRequestIndex: 2 }]; reading.items = [];
  const usage = { inputTokens: 0, outputTokens: 0, totalTokens: 0, cachedInputTokens: null, cacheWriteTokens: null, reasoningTokens: null, invalid: false };
  reading.responses = [{ requestId: 'ws', responseId: 'one', status: 'completed', reportedModels: ['<svg>'], usage, observedDurationMs: 0 }, { requestId: 'ws', responseId: 'two', status: 'receiving', reportedModels: [], usage: null }, { requestId: 'ws', responseId: 'three', status: 'failed', reportedModels: [], usage, usageConflict: true }];
  await show(); await click('用量与状态'); await click(root.querySelector('.wb-view-calls') as HTMLElement);
  const row = root.querySelector('.wb-call-row')!;
  expect(row.getAttribute('data-purpose')).toBe('unknown');
  expect(row.textContent).toContain('关联未确认'); expect(row.textContent).toContain('0 Token');
  expect(row.textContent).toContain('0.00 秒'); expect(row.textContent).toContain('Token 未提供');
  expect(row.textContent).toContain('用量待核对'); expect(row.textContent).toContain('响应失败');
  expect(row.querySelectorAll('.wb-call-metrics')).toHaveLength(3); expect(row.querySelector('svg')).toBeNull();
});

it('does not close current call details when an earlier history read finishes in the background', async () => {
  const old = { runEpoch: 'old', projectName: 'Past project', startedAt: '2026-09-20T00:00:00Z', state: 'ended' };
  let resolve!: (value: unknown) => void;
  const fetch = vi.fn().mockImplementation(async (url: string) => {
    if (url === '/workbench/v1/history') return success({ currentRunEpoch: 'live', runs: [old], nextCursor: null, diagnostics: [], indexState: 'ready' });
    if (url === '/workbench/v1/history/old') return new Promise(done => { resolve = done; });
    return success(detail('live', 'chat'));
  });
  vi.stubGlobal('fetch', fetch); await show(); await click('历史记录');
  await click(root.querySelector('.wb-history-list button') as HTMLElement);
  await click('实时对话'); await click('调用详情');
  const context = root.querySelector('.wb-context-entry');
  await act(async () => resolve(success({ currentRunEpoch: 'live', run: old, snapshot: snapshot('old'), uncertainTail: false, gaps: [], issues: [], detailsPartial: false })));
  await act(async () => {});
  expect(root.querySelector('.wb-saved-reading')).not.toBeNull();
  expect(root.querySelector('.wb-context-entry')).toBe(context);
  expect(root.querySelector('.wb-call-inspector')!.textContent).toContain('本次运行');
});
