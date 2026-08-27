import type { Item } from './types';

export type PresentationGroup = 'dialogue' | 'activity' | 'status' | 'request' | 'unknown';
export type DialogueRole = 'user' | 'assistant';
export type ActivityKind = 'command' | 'tool' | 'file' | 'search' | 'image' | 'plan' | 'collaboration'
  | 'reasoning' | 'usage' | 'compaction' | 'context' | 'error' | 'interrupt' | 'request' | 'other';

export interface ItemPresentation {
  group: PresentationGroup;
  label: string;
  role?: DialogueRole;
  activity?: ActivityKind;
}

export interface ActivityEntry {
  key: string;
  items: Item[];
  presentation: ItemPresentation;
  status: string;
}

const labels: Record<ActivityKind, string> = {
  command: '命令',
  tool: '工具调用',
  file: '文件修改',
  search: '搜索',
  image: '图片',
  plan: '计划',
  collaboration: '协作',
  reasoning: '推理摘要',
  usage: '用量',
  compaction: '上下文整理',
  context: '会话上下文',
  error: '错误',
  interrupt: '中断',
  request: '待处理请求',
  other: '其他过程'
};

function record(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null ? value as Record<string, unknown> : {};
}

export function itemPayload(item: Item): Record<string, unknown> {
  const raw = record(item.raw);
  return record(raw.payload ?? raw);
}

function activity(activity: ActivityKind, group: PresentationGroup = 'activity'): ItemPresentation {
  return { group, activity, label: labels[activity] };
}

const contextPrefixes = [
  '<app-context>',
  '<skills_instructions>',
  '<permissions instructions>',
  '<multi_agent_mode>',
  '<environment_context>',
  '<codex_internal_context',
  '# AGENTS.md instructions for ',
  '# Codex desktop context',
  'You are `/root`, the primary agent'
];

export function isContextMessage(item: Item): boolean {
  const summary = String(item.summaryText || '').trimStart();
  return contextPrefixes.some((prefix) => summary.startsWith(prefix));
}

export function presentItem(item: Item): ItemPresentation {
  const payload = itemPayload(item);
  const rawType = String(payload.type || '').toLowerCase();
  const type = item.itemType.toLowerCase();

  if (isContextMessage(item)) return activity('context', 'status');

  if (type === 'user_message' || (type === 'message' && payload.role === 'user')) {
    return { group: 'dialogue', role: 'user', label: '你' };
  }
  if (type === 'agent_message') {
    if (payload.author != null || payload.recipient != null) return activity('collaboration');
    return { group: 'dialogue', role: 'assistant', label: 'Codex' };
  }
  if (type === 'message') {
    return { group: 'dialogue', role: payload.role === 'user' ? 'user' : 'assistant', label: payload.role === 'user' ? '你' : 'Codex' };
  }

  if (['command_execution', 'local_shell_call', 'exec_command'].includes(type)) return activity('command');
  if (['file_change', 'patch', 'turn_diff'].includes(type)) return activity('file');
  if (['mcp_tool_call', 'tool_call', 'tool_output', 'function_call', 'function_call_output',
    'custom_tool_call', 'custom_tool_call_output', 'tool_search_call', 'tool_search_output', 'additional_tools'].includes(type)) return activity('tool');
  if (['web_search', 'web_search_call'].includes(type)) return activity('search');
  if (['image_generation', 'image_generation_call', 'view_image'].includes(type)) return activity('image');
  if (['collab', 'sub_agent', 'inter_agent_communication'].includes(type)) return activity('collaboration');
  if (type === 'plan' || rawType === 'plan_update') return activity('plan');
  if (type === 'reasoning') return activity('reasoning', 'status');
  if (type === 'usage' || rawType === 'token_count') return activity('usage', 'status');
  if (['compaction', 'compaction_summary', 'context_compaction', 'compaction_trigger', 'compacted'].includes(type)
    || ['compaction', 'compaction_summary', 'context_compaction', 'compaction_trigger'].includes(rawType)) return activity('compaction', 'status');
  if (type === 'error' || ['error', 'warning', 'stream_error'].includes(rawType)) return activity('error', 'status');
  if (['interrupt', 'completion'].includes(type) || ['turn_aborted', 'task_complete', 'turn_complete'].includes(rawType)) {
    return activity(type === 'interrupt' || rawType === 'turn_aborted' ? 'interrupt' : 'other', 'status');
  }
  if (['approval', 'user_question', 'request'].includes(type)
    || rawType.includes('approval') || rawType.includes('request_user_input')) return activity('request', 'request');

  // These official response variants can arrive before normalization catches up. Preserve them
  // as known process records instead of presenting a false compatibility warning.
  if (['additional_tools', 'tool_search_output', 'web_search_call', 'image_generation_call'].includes(rawType)) {
    return presentItem({ ...item, itemType: rawType });
  }
  return { group: 'unknown', activity: 'other', label: '未知类型' };
}

function activityKey(item: Item): string {
  const payload = itemPayload(item);
  const callId = payload.call_id ?? payload.callId ?? payload.request_id ?? payload.requestId;
  return callId == null || String(callId) === '' ? item.itemId : String(callId);
}

function statusRank(status: string) {
  if (['failed', 'error'].includes(status)) return 4;
  if (['pending', 'request', 'waiting'].includes(status)) return 3;
  if (['started', 'running', 'streaming'].includes(status)) return 2;
  return 1;
}

export function coalesceActivities(items: Item[]): ActivityEntry[] {
  const entries = new Map<string, ActivityEntry>();
  for (const item of items) {
    const presentation = presentItem(item);
    if (presentation.group === 'dialogue') continue;
    const key = activityKey(item);
    const existing = entries.get(key);
    if (!existing) {
      entries.set(key, { key, items: [item], presentation, status: item.status });
      continue;
    }
    existing.items.push(item);
    if (statusRank(item.status) >= statusRank(existing.status)) existing.status = item.status;
    if (existing.presentation.group === 'unknown' && presentation.group !== 'unknown') existing.presentation = presentation;
    if (presentation.activity === 'error' || presentation.activity === 'interrupt' || presentation.activity === 'request') {
      existing.presentation = presentation;
    }
  }
  return Array.from(entries.values());
}

export function summarizeActivities(entries: ActivityEntry[]): string {
  const counts = new Map<ActivityKind, number>();
  for (const entry of entries) {
    const kind = entry.presentation.activity || 'other';
    counts.set(kind, (counts.get(kind) || 0) + 1);
  }
  const order: ActivityKind[] = ['command', 'tool', 'file', 'search', 'image', 'plan', 'collaboration', 'reasoning',
    'usage', 'compaction', 'context', 'error', 'interrupt', 'request', 'other'];
  return order.filter((kind) => counts.has(kind)).map((kind) => `${counts.get(kind)} 次${labels[kind]}`).join(' · ');
}
