import { describe, expect, it } from 'vitest';
import { applyReadingEvent, conversationItems, conversationEntries, readSnapshot, keyOf, type ReadingView, type ReadingEvent, type RequestView } from './reading';
import { modelItems, toolItems, userMessages, parseViewItem, type ModelItem, type UserItem, type ToolItem } from './viewItems';
import type { ToolCall } from './toolTypes';

const toolFixture = (): ToolCall => ({
  key: { requestId: 'request', responseId: 'response', wireItemId: 'tool-1', contentIndex: 0 }, kind: 'tool_call', toolKind: 'custom', callId: 'call-1', name: 'exec', namespace: null,
  category: 'code', arguments: 'text(await tools.exec_command({cmd:"echo hi"}));', argumentsState: 'receiving', command: null, execution: 'unobserved', result: null,
  proposedPatch: null, revision: 1, orderIndex: 2, captureSeq: 5, truncated: false, identityConflict: false, resultConflict: false,
});

const key = { requestId: 'request-1', responseId: 'response-1', wireItemId: 'message-1', contentIndex: 0 };
const empty = (): ReadingView => ({ items: [], toolContexts: [], nativeCommands: [], nativeFileChanges: [], responses: [], requests: [], diagnostics: [], userCapture: { enabled: false, diagnostics: [] }, capture: 'ok', connected: true, viewSeq: 0 });
const model = (text = '正文', itemKey = 'model:1', requestId = 'request-1'): ModelItem => ({ kind: 'message', itemKey, revision: 1, orderIndex: 1, completeness: 'observed', truncated: false, author: { role: 'assistant', requestedModel: null, reportedModels: [] }, content: [{ contentKey: '0', text }], streamState: 'receiving', evidence: [{ origin: 'model', key: { ...key, requestId }, captureSeq: 1 }] });
const user = (turn: string, id: string): UserItem => ({ kind: 'message', itemKey: `user:${id}`, revision: 1, orderIndex: 9, completeness: 'observed', truncated: false, omitted: false, author: { role: 'user' }, content: [{ contentKey: 'text', text: '相同输入' }], streamState: 'ended', evidence: [{ origin: 'rollout', key: { codexThreadId: 'thread', codexTurnId: turn, nativeItemId: id }, source: { sourceRef: 'source', byteOffset: 100, ordinal: 1 } }] });
const tool = (): ToolItem => ({ ...toolFixture(), itemKey: 'tool:1', revision: 1, orderIndex: 3, completeness: 'observed', evidence: [{ origin: 'model', key, captureSeq: 1 }] });
const request = (id = 'request-1', turn = 'turn-1'): RequestView => ({ requestId: id, clientRequestIndex: null, purpose: 'conversation', purposeBasis: 'codex_turn_metadata', requestedModel: 'model', codexThreadId: 'thread', codexTurnId: turn });
const replace = (viewSeq: number, item: ReadingView['items'][number]): ReadingEvent => ({ kind: 'item.replace', viewSeq, item });
const patch = (viewSeq: number, baseRevision: number, append: string): ReadingEvent => ({ kind: 'item.patch', field: 'text', contentKey: '0', itemKey: 'model:1', viewSeq, baseRevision, revision: baseRevision + 1, append, truncated: false });
const unsafeEvent = (event: unknown) => event as ReadingEvent;

describe('unified reading stream', () => {
  it('starts with a typed replacement, applies deltas once, and replaces the final content in place', () => {
    expect(() => applyReadingEvent(empty(), patch(1, 0, 'cannot invent role'))).toThrow();
    let state = applyReadingEvent(empty(), replace(1, model('中文')));
    state = applyReadingEvent(state, replace(1, model('duplicate')));
    state = applyReadingEvent(state, patch(2, 1, '\n正文'));
    expect(modelItems(state)[0].text).toBe('中文\n正文');
    state = applyReadingEvent(state, replace(3, { ...model('最终'), revision: 3, streamState: 'ended' }));
    state = applyReadingEvent(state, replace(4, model('stale')));
    expect(state.items).toHaveLength(1); expect(modelItems(state)[0].text).toBe('最终');
    expect(state.viewSeq).toBe(4);
  });
  it('rejects revision, content, role, field, sequence and type mismatches without altering text', () => {
    const state = applyReadingEvent(empty(), replace(1, model('first')));
    for (const event of [patch(3, 1, 'missing'), patch(2, 5, 'missing'), { ...patch(2, 1, 'wrong'), contentKey: 'other' }, { ...patch(2, 1, 'wrong'), field: 'arguments' }, { kind: 'unknown', viewSeq: 2 }, replace(2, { ...user('turn', 'item'), itemKey: 'model:1', revision: 2 })]) expect(() => applyReadingEvent(state, unsafeEvent(event))).toThrow();
    expect(modelItems(state)[0].text).toBe('first');
  });
  it('keeps distinct content indices and patches only the explicitly identified part', () => {
    const state = applyReadingEvent(empty(), replace(1, { ...model('zero'), content: [{ contentKey: '0', text: 'zero' }, { contentKey: '1', text: 'one' }] }));
    const next = applyReadingEvent(state, unsafeEvent({ ...patch(2, 1, '!'), contentKey: '1' }));
    expect((next.items[0] as ModelItem).content.map(part => part.text)).toEqual(['zero', 'one!']);
    expect(keyOf({ ...key, requestId: 'a:b', responseId: 'c' })).not.toBe(keyOf({ ...key, requestId: 'a', responseId: 'b:c' }));
  });
  it('resumes a typed snapshot at its cursor with identical cards to an uninterrupted stream', () => {
    let live = applyReadingEvent(empty(), replace(1, model('before')));
    live = applyReadingEvent(live, replace(2, tool()));
    let resumed = readSnapshot({ ...live, schemaVersion: 2, runEpoch: 'epoch' }, 'epoch');
    const events: ReadingEvent[] = [patch(3, 1, ' after'), { kind: 'item.patch', field: 'arguments', itemKey: 'tool:1', viewSeq: 4, baseRevision: 1, revision: 2, append: ' partial', truncated: false }, replace(5, { ...tool(), revision: 3, arguments: 'final', argumentsState: 'generated' })];
    for (const event of events) { live = applyReadingEvent(live, event); resumed = applyReadingEvent(resumed, event); }
    expect(resumed.items).toEqual(live.items); expect(resumed.viewSeq).toBe(live.viewSeq);
    expect(() => readSnapshot({ ...live, schemaVersion: 1, runEpoch: 'epoch' }, 'epoch')).toThrow();
    expect(() => readSnapshot({ ...live, schemaVersion: 2, runEpoch: 'old' }, 'epoch')).toThrow();
    expect(() => readSnapshot({ ...live, schemaVersion: 2, runEpoch: 'epoch', items: [model(), model()] }, 'epoch')).toThrow();
  });
  it('preserves interleaved model/tool order and replaces results in the original card', () => {
    let state = applyReadingEvent(empty(), { kind: 'request.metadata', viewSeq: 1, request: request() });
    state = applyReadingEvent(state, replace(2, model('before')));
    state = applyReadingEvent(state, replace(3, { ...tool(), key }));
    state = applyReadingEvent(state, replace(4, { ...model('after', 'model:2'), orderIndex: 4 }));
    const entries = conversationEntries(state);
    expect(entries.map(entry => entry.kind)).toEqual(['model', 'tool', 'model']);
    state = applyReadingEvent(state, replace(5, { ...tool(), key, revision: 2, argumentsState: 'generated', execution: 'failed', result: { source: { kind: 'native_rollout', sourceRef: 'source', byteOffset: 90, nativeItemId: 'call', processId: null }, output: 'failure', exitCode: 7, durationMs: null, omitted: false, truncated: false } }));
    expect(conversationEntries(state).map(entry => entry.key)).toEqual(entries.map(entry => entry.key));
    expect(toolItems(state)).toHaveLength(1); expect(toolItems(state)[0].execution).toBe('failed');
  });
  it('inserts delayed native submissions in the proven turn and keeps identical new submissions', () => {
    let state = applyReadingEvent(empty(), { kind: 'request.metadata', viewSeq: 1, request: request() });
    state = applyReadingEvent(state, replace(2, model('first')));
    state = applyReadingEvent(state, { kind: 'request.metadata', viewSeq: 3, request: request('request-2', 'turn-2') });
    state = applyReadingEvent(state, replace(4, model('second', 'model:2', 'request-2')));
    state = applyReadingEvent(state, replace(5, user('turn-2', 'user-2')));
    state = applyReadingEvent(state, replace(6, user('turn-1', 'user-1')));
    const entries = conversationEntries(state);
    expect(entries.map(entry => entry.kind)).toEqual(['user', 'model', 'user', 'model']);
    expect(entries[0].key).not.toBe(entries[2].key);
    state = applyReadingEvent(state, replace(7, user('turn-1', 'user-1')));
    state = applyReadingEvent(state, replace(8, user('unrelated-turn', 'user-3')));
    expect(conversationEntries(state)).toEqual(entries); expect(userMessages(state)).toHaveLength(3);
  });
  it('does not infer auxiliary, missing metadata or WS create/response relationships from text', () => {
    let state = applyReadingEvent(empty(), replace(1, model('ordinary prose or title JSON')));
    expect(conversationItems(state)).toEqual([]);
    state = applyReadingEvent(state, { kind: 'request.metadata', viewSeq: 2, request: { ...request(), purpose: 'auxiliary' } });
    expect(conversationItems(state)).toEqual([]);
    state = applyReadingEvent(state, { kind: 'request.metadata', viewSeq: 3, request: request() });
    expect(conversationItems(state)).toHaveLength(1);
    expect(conversationEntries({ ...state, requests: [{ ...request(), clientRequestIndex: 1 }] })).toEqual([]);
  });
  it('keeps unknown variants as safe notices and continues processing subsequent events', () => {
    const unknown = { kind: 'future_variant', itemKey: 'future:1', revision: 1, orderIndex: 1, payload: '<img src=x onerror=alert(1)>', author: { role: 'user' } };
    let state = applyReadingEvent(empty(), unsafeEvent({ kind: 'item.replace', viewSeq: 1, item: unknown }));
    expect(state.items[0].kind).toBe('notice'); expect(JSON.stringify(state.items)).not.toContain('onerror');
    expect(conversationEntries(state)).toEqual([]); expect(state.capture).toBe('partial');
    state = applyReadingEvent(state, replace(2, model('continues')));
    expect(modelItems(state)[0].text).toBe('continues');
    const snapshot = readSnapshot({ ...empty(), schemaVersion: 2, runEpoch: 'epoch', viewSeq: 1, items: [unknown] }, 'epoch');
    expect(snapshot.items[0]).toEqual(state.items[0]);
    expect(() => parseViewItem({ ...model(), content: [{ contentKey: '0', text: 'a' }, { contentKey: '0', text: 'b' }] })).toThrow();
  });
  it('keeps independent conflicting native sources without creating user or model messages', () => {
    const change = { key: { codexThreadId: 'thread', codexTurnId: 'turn', nativeItemId: 'call' }, source: { sourceRef: 'source', byteOffset: 50, ordinal: 1 }, status: 'failed' as const, files: [], stdout: '', stderr: 'error', truncated: false, omitted: false };
    let state = applyReadingEvent(empty(), { kind: 'native.file_change', viewSeq: 1, change });
    state = applyReadingEvent(state, { kind: 'native.file_change', viewSeq: 2, change });
    expect(state.nativeFileChanges).toHaveLength(1);
    state = applyReadingEvent(state, { kind: 'native.file_change', viewSeq: 3, change: { ...change, status: 'completed', source: { ...change.source, byteOffset: 90 } } });
    expect(state.nativeFileChanges).toHaveLength(2); expect(state.items).toHaveLength(0);
    const command = { ...change, processId: null, commandSource: 'agent', command: ['echo'], cwd: '/', output: '', exitCode: 1, durationMs: null };
    state = applyReadingEvent(state, { kind: 'native.command', viewSeq: 4, command });
    state = applyReadingEvent(state, { kind: 'native.command', viewSeq: 5, command });
    expect(state.nativeCommands).toHaveLength(1);
  });
  it('retains capture diagnostics without modifying text', () => {
    let state = applyReadingEvent(empty(), replace(1, model('first')));
    state = applyReadingEvent(state, { kind: 'capture.gap', viewSeq: 2, diagnostic: { requestId: 'request-1', captureSeq: 7, code: 'unknown_event' } });
    state = applyReadingEvent(state, { kind: 'user.capture', viewSeq: 3, userCapture: { enabled: true, diagnostics: [{ sourceRef: 'source', byteOffset: 200, code: 'source_changed' }] } });
    expect(modelItems(state)[0].text).toBe('first'); expect(state.capture).toBe('partial');
    expect(state.diagnostics).toHaveLength(1); expect(state.userCapture.diagnostics).toHaveLength(1);
  });
});
