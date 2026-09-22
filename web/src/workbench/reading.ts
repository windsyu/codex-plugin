import { useEffect, useState } from 'preact/hooks';
import { modelItems, toolItems, userMessages, parseViewItem, sameItemType, VIEW_SCHEMA_VERSION, type ViewItem, type ViewBase, type ModelItem } from './viewItems';
import type { NativeCommand, NativeFileChange, ToolCall, ToolContext } from './toolTypes';
import { parseUsageSummary, type UsageSummary } from './usage';

export interface TextKey { requestId: string; responseId: string | null; wireItemId: string; contentIndex: number }
export interface TextItem { key: TextKey; text: string; revision: number; truncated: boolean; orderIndex?: number; itemKey?: string; author?: ModelItem['author'] }
export interface ResponseUsage {
  inputTokens: number | null; outputTokens: number | null; totalTokens: number | null;
  cachedInputTokens: number | null; cacheWriteTokens: number | null; reasoningTokens: number | null; invalid: boolean;
}
export interface ResponseView {
  requestId: string; responseId: string | null; status: string; reportedModels: string[];
  usage?: ResponseUsage | null; usageConflict?: boolean; observedDurationMs?: number | null;
}
export interface RequestView {
  requestId: string; clientRequestIndex: number | null; requestedModel: string | null;
  codexThreadId: string | null; codexTurnId: string | null;
  purpose: 'conversation' | 'auxiliary' | 'unknown';
  purposeBasis: 'codex_turn_metadata' | 'missing_metadata' | 'conflicting_metadata' | 'unknown_metadata';
}
export interface Diagnostic { requestId: string; captureSeq: number; code: string }
export interface UserKey { codexThreadId: string; codexTurnId: string; nativeItemId: string }
export interface UserRecord {
  key: UserKey; role: 'user'; text: string; revision: number; truncated: boolean; omitted: boolean;
  source: { sourceRef: string; byteOffset: number; ordinal: number | null };
}
export interface UserCapture { enabled: boolean; diagnostics: { sourceRef: string; byteOffset: number; code: string }[] }
export interface ReadingView {
  usageSummary?: UsageSummary;
  recorderStatus?: RecorderStatus;
  items: ViewItem[]; responses: ResponseView[]; requests: RequestView[]; diagnostics: Diagnostic[];
  toolContexts: ToolContext[];
  nativeCommands: NativeCommand[];
  nativeFileChanges: NativeFileChange[];
  userCapture: UserCapture;
  capture: string; connected: boolean; viewSeq: number;
}
export interface RecorderStatus {
  runEpoch: string; state: 'disabled' | 'pending' | 'saved' | 'degraded';
  persistedThroughViewSeq: number; savedThroughViewSeq: number; observedViewSeq: number;
  historyCoverage: 'partial' | 'complete_for_observed_scope'; gapCount: number; error: string | null;
}
export function applyRecorderStatus(state: ReadingView, value: RecorderStatus, epoch: string): ReadingView {
  if (value.runEpoch !== epoch || !['disabled', 'pending', 'saved', 'degraded'].includes(value.state)
    || ![value.persistedThroughViewSeq, value.savedThroughViewSeq, value.observedViewSeq, value.gapCount].every(v => Number.isSafeInteger(v) && v >= 0)
    || value.persistedThroughViewSeq > value.savedThroughViewSeq) throw new Error('invalid recorder status');
  return { ...state, recorderStatus: value };
}
export const keyOf = (key: TextKey) => JSON.stringify([key.requestId, key.responseId, key.wireItemId, key.contentIndex]);
const responseKey = (r: ResponseView) => JSON.stringify([r.requestId, r.responseId]);
const empty = (): ReadingView => ({ items: [], toolContexts: [], nativeCommands: [], nativeFileChanges: [], responses: [], requests: [], diagnostics: [], userCapture: { enabled: false, diagnostics: [] }, capture: 'unavailable', connected: false, viewSeq: 0 });
export const requestFor = (state: ReadingView, item: { key: TextKey }) => state.requests.find(request => request.requestId === item.key.requestId && request.clientRequestIndex === null);
export const conversationItems = (state: ReadingView) => modelItems(state).filter(item => requestFor(state, item)?.purpose === 'conversation');
export const userKeyOf = (key: UserKey) => JSON.stringify([key.codexThreadId, key.codexTurnId, key.nativeItemId]);
export type ConversationEntry = { kind: 'user'; key: string; user: UserRecord; orderUnconfirmed: boolean } | { kind: 'model'; key: string; item: TextItem; request: RequestView } | { kind: 'tool'; key: string; tool: ToolCall; request: RequestView };
export function conversationEntries(state: ReadingView): ConversationEntry[] {
  const entries: ConversationEntry[] = [], turns = new Set<string>();
  for (const request of state.requests) {
    if (request.purpose !== 'conversation' || request.clientRequestIndex !== null) continue;
    const turn = JSON.stringify([request.codexThreadId, request.codexTurnId]);
    if (turns.has(turn)) continue;
    turns.add(turn);
    // Native byte positions order known user records within this source. The
    // explicit turn relation, never text or arrival time, places late evidence.
    const users = userMessages(state).filter(user => user.key.codexThreadId === request.codexThreadId && user.key.codexTurnId === request.codexTurnId);
    users.sort((a, b) => a.source.sourceRef === b.source.sourceRef ? a.source.byteOffset - b.source.byteOffset : 0);
    for (const user of users) entries.push({ kind: 'user', key: user.itemKey, user, orderUnconfirmed: users.length > 1 });
    const content = [...modelItems(state).map(item => ({ kind: 'model' as const, item, order: item.orderIndex || 0 })), ...toolItems(state).map(tool => ({ kind: 'tool' as const, item: tool, order: tool.orderIndex }))].sort((a, b) => a.order - b.order);
    for (const entry of content) {
      const matched = requestFor(state, entry.item);
      if (matched?.purpose === 'conversation' && matched.codexThreadId === request.codexThreadId && matched.codexTurnId === request.codexTurnId) {
        entries.push(entry.kind === 'model' ? { kind: 'model', key: entry.item.itemKey!, item: entry.item, request: matched } : { kind: 'tool', key: entry.item.itemKey, tool: entry.item, request: matched });
      }
    }
  }
  return entries;
}

type ReadingEventBase = { viewSeq: number; usageSummary?: UsageSummary };
export type ReadingEvent = ReadingEventBase & (
  | { kind: 'item.replace'; item: ViewItem }
  | ({ kind: 'item.patch'; itemKey: string; baseRevision: number; revision: number; truncated: boolean } & ({ field: 'text'; contentKey: string; append: string } | { field: 'arguments'; append: string }))
  | { kind: 'tool.context'; context: ToolContext }
  | { kind: 'request.state'; response: ResponseView }
  | { kind: 'request.metadata'; request: RequestView }
  | { kind: 'native.command'; command: NativeCommand }
  | { kind: 'native.file_change'; change: NativeFileChange }
  | { kind: 'user.capture'; userCapture: UserCapture }
  | { kind: 'capture.gap'; diagnostic: Diagnostic }
);
export function readSnapshot(snapshot: any, epoch: string): ReadingView {
  if (snapshot.runEpoch !== epoch || snapshot.schemaVersion !== VIEW_SCHEMA_VERSION || !Number.isSafeInteger(snapshot.viewSeq) || snapshot.viewSeq < 0 || !Array.isArray(snapshot.items)) throw new Error('incompatible reading snapshot');
  const items = snapshot.items.map(parseViewItem) as ViewItem[];
  if (new Set(items.map(item => item.itemKey)).size !== items.length) throw new Error('duplicate reading item');
  return { ...empty(), ...snapshot, usageSummary: parseUsageSummary(snapshot.usageSummary), items, connected: false, capture: items.some(item => item.completeness !== 'observed') ? 'partial' : snapshot.capture };
}

export function applyReadingEvent(state: ReadingView, event: ReadingEvent): ReadingView {
  if (event.viewSeq <= state.viewSeq) return state;
  if (event.viewSeq !== state.viewSeq + 1) throw new Error('reading sequence gap');
  const usageSummary = event.usageSummary === undefined ? state.usageSummary : parseUsageSummary(event.usageSummary);
  let items = state.items, responses = state.responses, requests = state.requests, diagnostics = state.diagnostics, capture = state.capture;
  let userCapture = state.userCapture;
  let toolContexts = state.toolContexts;
  let nativeCommands = state.nativeCommands;
  let nativeFileChanges = state.nativeFileChanges;
  if (event.kind === 'item.replace') {
    const item = parseViewItem(event.item);
    const index = items.findIndex(previous => previous.itemKey === item.itemKey), previous = items[index];
    if (previous && !sameItemType(previous, item)) throw new Error('reading item type conflict');
    if (!previous || item.revision > previous.revision) {
      items = [...items]; if (index < 0) items.push(item); else items[index] = item;
    }
    if (item.completeness !== 'observed') capture = 'partial';
  } else if (event.kind === 'item.patch') {
    const index = items.findIndex(item => item.itemKey === event.itemKey), previous = items[index];
    if (!previous || previous.revision !== event.baseRevision || event.revision !== event.baseRevision + 1 || typeof event.append !== 'string') throw new Error('reading revision gap');
    let item: ViewItem;
    if (event.field === 'text' && previous.kind === 'message' && previous.author.role === 'assistant') {
      if (!previous.content.some(part => part.contentKey === event.contentKey)) throw new Error('reading content gap');
      item = { ...previous, content: previous.content.map(part => part.contentKey === event.contentKey ? { ...part, text: part.text + event.append } : part) };
    } else if (event.field === 'arguments' && previous.kind === 'tool_call') {
      item = { ...previous, arguments: previous.arguments + event.append };
    } else throw new Error('reading patch type conflict');
    item = { ...item, revision: event.revision, truncated: event.truncated, completeness: event.truncated ? 'partial' : item.completeness };
    items = [...items]; items[index] = item;
    if (item.completeness !== 'observed') capture = 'partial';
  } else if (event.kind === 'tool.context') {
    toolContexts = [...toolContexts.filter(context => context.requestId !== event.context.requestId || context.clientRequestIndex !== event.context.clientRequestIndex), event.context];
    if (event.context.partial) capture = 'partial';
  } else if (event.kind === 'request.state') {
    responses = [...responses.filter(response => responseKey(response) !== responseKey(event.response)), event.response];
    if (event.response.reportedModels?.length > 1 || event.response.usageConflict || event.response.usage?.invalid) capture = 'partial';
  } else if (event.kind === 'request.metadata') {
    requests = [...requests];
    const index = requests.findIndex(request => request.requestId === event.request.requestId && request.clientRequestIndex === event.request.clientRequestIndex);
    if (index < 0) requests.push(event.request); else requests[index] = event.request;
  } else if (event.kind === 'native.command') {
    const command: NativeCommand = event.command;
    nativeCommands = [...nativeCommands.filter(existing => existing.source.sourceRef !== command.source.sourceRef || existing.source.byteOffset !== command.source.byteOffset), command];
  } else if (event.kind === 'native.file_change') {
    const change: NativeFileChange = event.change;
    nativeFileChanges = [...nativeFileChanges.filter(existing => existing.source.sourceRef !== change.source.sourceRef || existing.source.byteOffset !== change.source.byteOffset), change];
  } else if (event.kind === 'user.capture') {
    userCapture = event.userCapture;
    if (userCapture.diagnostics.length) capture = 'partial';
  } else if (event.kind === 'capture.gap') {
    const diagnostic: Diagnostic = event.diagnostic;
    diagnostics = [...diagnostics.filter(value => value.requestId !== diagnostic.requestId || value.code !== diagnostic.code), diagnostic];
    capture = 'partial';
  } else throw new Error('unknown reading event');
  return { ...state, viewSeq: event.viewSeq, usageSummary, items, toolContexts, nativeCommands, nativeFileChanges, responses, requests, diagnostics, userCapture, capture };
}

export function useReading(epoch: string): ReadingView {
  const [view, setView] = useState<ReadingView>(empty);
  useEffect(() => {
    let state = empty(), disposed = false, source: EventSource | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined, frame = 0;
    const publish = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => { if (!disposed) setView(state); });
    };
    const reconnect = () => {
      source?.close(); state = { ...state, connected: false }; publish();
      if (!disposed && !retry) retry = setTimeout(() => { retry = undefined; void connect(); }, 500);
    };
    const connect = async () => {
      try {
        const response = await fetch('/workbench/v1/live/snapshot', { credentials: 'same-origin', cache: 'no-store' });
        if (!response.ok) throw new Error('reading unavailable');
        const snapshot = await response.json();
        if (disposed) return;
        if (snapshot.runEpoch !== epoch) throw new Error('reading run changed');
        state = readSnapshot(snapshot, epoch); publish();
        source = new EventSource(`/workbench/v1/live/events?epoch=${encodeURIComponent(epoch)}&after=${snapshot.viewSeq}`);
        source.onopen = () => { state = { ...state, connected: true }; publish(); };
        source.addEventListener('view', event => {
          try {
            const value = JSON.parse((event as MessageEvent).data);
            if (value.runEpoch !== epoch) throw new Error('old reading epoch');
            state = applyReadingEvent(state, value as ReadingEvent); publish();
          } catch { reconnect(); }
        });
        source.addEventListener('snapshot_required', reconnect); source.onerror = reconnect;
        source.addEventListener('recorder.status', event => {
          try { state = applyRecorderStatus(state, JSON.parse((event as MessageEvent).data), epoch); publish(); }
          catch { reconnect(); }
        });
      } catch { reconnect(); }
    };
    void connect();
    return () => { disposed = true; source?.close(); clearTimeout(retry); cancelAnimationFrame(frame); };
  }, [epoch]);
  return view;
}
