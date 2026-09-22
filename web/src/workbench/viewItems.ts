import type { TextItem, TextKey, UserKey, UserRecord, ReadingView } from './reading';
import type { ToolCall } from './toolTypes';

export const VIEW_SCHEMA_VERSION = 2;
export interface ViewBase {
  itemKey: string; revision: number; orderIndex: number;
  completeness: 'observed' | 'partial' | 'omitted'; truncated: boolean;
}
export interface ModelEvidence { origin: 'model'; key: TextKey; captureSeq: number }
export interface RolloutEvidence { origin: 'rollout'; key: UserKey; source: UserRecord['source'] }
export interface DiagnosticEvidence { origin: 'diagnostic'; requestId: string; captureSeq: number }
interface MessageBase extends ViewBase { kind: 'message'; content: { contentKey: string; text: string }[]; streamState: 'receiving' | 'ended' | 'incomplete' }
export interface ModelItem extends MessageBase { author: { role: 'assistant'; requestedModel: string | null; reportedModels: string[] }; evidence: [ModelEvidence] }
export interface UserItem extends MessageBase { author: { role: 'user' }; evidence: [RolloutEvidence]; omitted: boolean }
export type ToolItem = ToolCall & ViewBase & { evidence: [ModelEvidence] };
export interface NoticeItem extends ViewBase { kind: 'notice'; code: 'unassigned' | 'unknown_type' | 'capture_gap' | 'omitted'; text: string; evidence: DiagnosticEvidence[] }
export type ViewItem = ModelItem | UserItem | ToolItem | NoticeItem;
export const isModelItem = (item: ViewItem): item is ModelItem => item.kind === 'message' && item.author.role === 'assistant';
export const isUserItem = (item: ViewItem): item is UserItem => item.kind === 'message' && item.author.role === 'user';
export const isToolItem = (item: ViewItem): item is ToolItem => item.kind === 'tool_call';
export const modelItems = (state: Pick<ReadingView, 'items'>): TextItem[] => state.items.filter(isModelItem).map(item => ({ ...item, key: item.evidence[0].key, text: item.content.map(content => content.text).join('\n') }));
export const toolItems = (state: Pick<ReadingView, 'items'>) => state.items.filter(isToolItem);
export const userMessages = (state: Pick<ReadingView, 'items'>): (UserRecord & ViewBase)[] => state.items.filter(isUserItem).map(item => ({ ...item, role: 'user', key: item.evidence[0].key, source: item.evidence[0].source, text: item.content.map(content => content.text).join('\n') }));
export function itemRequestId(item: ViewItem): string | null {
  const evidence = item.evidence[0];
  return evidence?.origin === 'model' ? evidence.key.requestId : evidence?.origin === 'diagnostic' ? evidence.requestId : null;
}

// An unknown discriminator must not repeatedly break the stream or expose its
// payload as a guessed message. Its stable envelope becomes a literal notice.
export function parseViewItem(value: unknown): ViewItem {
  if (!value || typeof value !== 'object') throw new Error('invalid reading item');
  const item = value as Record<string, any>;
  if (typeof item.itemKey !== 'string' || !item.itemKey.length || item.itemKey.length > 8192 || !Number.isSafeInteger(item.revision) || item.revision < 1 || !Number.isSafeInteger(item.orderIndex) || item.orderIndex < 0) throw new Error('invalid reading identity');
  const knownRole = item.kind === 'message' && ['assistant', 'user'].includes(item.author?.role);
  if (!knownRole && item.kind !== 'tool_call' && item.kind !== 'notice') {
    return { kind: 'notice', itemKey: item.itemKey, revision: item.revision, orderIndex: item.orderIndex, truncated: false, completeness: 'partial', code: 'unknown_type', text: '未识别的内容类型；未将其解释为用户、模型或工具。', evidence: [] };
  }
  if (!['observed', 'partial', 'omitted'].includes(item.completeness) || typeof item.truncated !== 'boolean' || !Array.isArray(item.evidence)) throw new Error('invalid reading envelope');
  const evidence = item.evidence[0];
  if (item.kind === 'message') {
    if (!Array.isArray(item.content) || !item.content.length || item.content.length > 64 || !item.content.every((part: any) => typeof part.contentKey === 'string' && typeof part.text === 'string') || new Set(item.content.map((part: any) => part.contentKey)).size !== item.content.length || !['receiving', 'ended', 'incomplete'].includes(item.streamState)) throw new Error('invalid message content');
    if (item.author.role === 'assistant' && (evidence?.origin !== 'model' || !validTextKey(evidence.key) || !Array.isArray(item.author.reportedModels) || !item.author.reportedModels.every((model: unknown) => typeof model === 'string') || !(item.author.requestedModel === null || typeof item.author.requestedModel === 'string'))) throw new Error('invalid model evidence');
    if (item.author.role === 'user' && (evidence?.origin !== 'rollout' || typeof evidence.key?.codexThreadId !== 'string' || typeof evidence.key?.codexTurnId !== 'string' || typeof evidence.key?.nativeItemId !== 'string' || typeof evidence.source?.sourceRef !== 'string' || !Number.isSafeInteger(evidence.source?.byteOffset) || typeof item.omitted !== 'boolean')) throw new Error('invalid user evidence');
  } else if (item.kind === 'tool_call') {
    if (evidence?.origin !== 'model' || !validTextKey(item.key) || !validTextKey(evidence.key) || typeof item.arguments !== 'string' || !['receiving', 'generated', 'incomplete'].includes(item.argumentsState)) throw new Error('invalid tool evidence');
  } else if (typeof item.text !== 'string' || !['unassigned', 'unknown_type', 'capture_gap', 'omitted'].includes(item.code)) throw new Error('invalid notice');
  return item as ViewItem;
}
function validTextKey(key: any): boolean {
  return !!key && typeof key.requestId === 'string' && (key.responseId === null || typeof key.responseId === 'string') && typeof key.wireItemId === 'string' && Number.isSafeInteger(key.contentIndex) && key.contentIndex >= 0;
}
export function sameItemType(a: ViewItem, b: ViewItem): boolean {
  return a.kind === b.kind && (a.kind !== 'message' || b.kind === 'message' && a.author.role === b.author.role);
}
