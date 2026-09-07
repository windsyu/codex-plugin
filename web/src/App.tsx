import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import { Api, ApiError, connect, loadDashboard, loadEventPage, loadThread, reconnectingStream } from './api';
import { renderMarkdown } from './lib/markdown';
import { coalesceActivities, itemPayload, presentItem, summarizeActivities } from './presentation';
import { SessionShell } from './session/SessionShell';
import type { SessionIntent } from './session/SessionShell';
import type { ActivityEntry } from './presentation';
import type { ControlCatalog, ControllerSource, GatewayCommand, GatewayStatusCard, Health, Item, PendingRequest, ProjectSummary, RawEvent, SearchResult, Source, Thread, ThreadDetail, Turn } from './types';

type RequestPhase = 'idle' | 'loading' | 'success' | 'error';
type TransportState = 'connecting' | 'live' | 'disconnected';
interface Filters { source: string; status: string; completeness: string; archived: string; q: string; }
interface RequestState { phase: RequestPhase; message?: string; }
interface NewThreadInput { sourceId: string; sourceEpoch: string; cwd: string; model: string | null; personality: string | null; permissions: string | null; }

const defaultFilters: Filters = { source: '', status: '', completeness: '', archived: '', q: '' };
const savedToken = sessionStorage.getItem('observer-token') || '';
const tailscaleViewer = window.location.protocol === 'https:' && window.location.hostname.endsWith('.ts.net');

function Icon({ name, size = 16 }: { name: 'codex' | 'folder' | 'search' | 'chevron' | 'arrow' | 'down' | 'plus' | 'stop' | 'send' | 'close' | 'settings'; size?: number }) {
  const paths = {
    codex: <><path d="M8 1.5 13.6 4.7v6.6L8 14.5l-5.6-3.2V4.7L8 1.5Z"/><path d="m5.2 6.1 2.8-1.6 2.8 1.6v3.8L8 11.5 5.2 9.9V6.1Z"/></>,
    folder: <path d="M1.8 4.1h4.6l1.3 1.5h6.5v7.2H1.8V4.1Z"/>,
    search: <><circle cx="7" cy="7" r="4.3"/><path d="m10.2 10.2 3.3 3.3"/></>,
    chevron: <path d="m6 3.5 4.5 4.5L6 12.5"/>,
    arrow: <><path d="M13.5 8h-11M6.5 4 2.5 8l4 4"/></>,
    down: <><path d="M8 2.5v10M3.8 8.5 8 12.7l4.2-4.2"/></>,
    plus: <><path d="M8 2.5v11M2.5 8h11"/></>,
    stop: <rect x="4.5" y="4.5" width="7" height="7" rx="1" fill="currentColor" stroke="none" />,
    send: <><path d="M8 13.5v-11M3.5 7 8 2.5 12.5 7"/></>,
    close: <><path d="m4 4 8 8M12 4l-8 8"/></>,
    settings: <><path d="M2.5 4.5h11M5.5 4.5a1.5 1.5 0 1 0-3 0 1.5 1.5 0 0 0 3 0ZM2.5 11.5h11M13.5 11.5a1.5 1.5 0 1 0-3 0 1.5 1.5 0 0 0 3 0Z"/></>
  };
  return <svg class={`icon icon-${name}`} width={size} height={size} viewBox="0 0 16 16" aria-hidden="true"
    fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">{paths[name]}</svg>;
}

function formatTime(ms?: number) {
  return ms ? new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'medium' }).format(new Date(ms)) : '时间未知';
}

function formatClock(ms?: number) {
  return ms ? new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }).format(new Date(ms)) : '时间未知';
}

function phaseLabel(value: string) {
  return ({ commentary: '进度更新', final: '最终回复', message: '消息' } as Record<string, string>)[value]
    || value.replaceAll('_', ' ');
}

function formatRelativeTime(ms?: number) {
  if (!ms) return '时间未知';
  const delta = ms - Date.now();
  const abs = Math.abs(delta);
  const [value, unit] = abs < 60_000 ? [Math.round(delta / 1000), 'second']
    : abs < 3_600_000 ? [Math.round(delta / 60_000), 'minute']
      : abs < 86_400_000 ? [Math.round(delta / 3_600_000), 'hour'] : [Math.round(delta / 86_400_000), 'day'];
  return new Intl.RelativeTimeFormat('zh-CN', { numeric: 'auto' }).format(value, unit as Intl.RelativeTimeFormatUnit);
}

function duration(start?: number, end?: number) {
  if (start == null || end == null || end < start) return undefined;
  const value = end - start;
  return value < 1000 ? `${value} ms` : `${(value / 1000).toFixed(value < 10_000 ? 1 : 0)} s`;
}

function statusLabel(value: string | undefined) {
  return ({ active: '活跃', idle: '空闲', not_loaded: '未加载', completed: '已完成', running: '进行中', streaming: '生成中',
    failed: '失败', error: '错误', pending: '待处理', interrupted: '已中断' } as Record<string, string>)[value || '']
    || value?.replaceAll('_', ' ') || '状态未知';
}

function badge(value: string | undefined, label?: string) {
  const normalized = value || 'unknown';
  const display = ({ completed: '已完成', running: '进行中', streaming: '生成中', failed: '失败', error: '错误', pending: '待处理',
    durable_complete: '历史完整', durable_partial: '历史不完整', live_complete: '实时完整', live_partial: '实时不完整',
    metadata_only: '仅元数据', ephemeral_lost: '实时细节已丢失', unknown: '未知' } as Record<string, string>)[normalized]
    || normalized.replaceAll('_', ' ');
  return <span class={`badge badge-${normalized}`} aria-label={label ? `${label}：${normalized}` : normalized}>
    {display}
  </span>;
}

function jsonText(value: unknown) {
  return value == null ? '' : typeof value === 'string' ? value : JSON.stringify(value, null, 2);
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null ? value as Record<string, unknown> : {};
}

function payloadOf(item: Item) {
  return itemPayload(item);
}

function textValue(value: unknown): string {
  if (value == null) return '';
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(textValue).filter(Boolean).join('\n');
  if (typeof value === 'object') {
    const object = record(value);
    return textValue(object.text ?? object.content ?? object.message) || jsonText(value);
  }
  return String(value);
}

function itemSummary(item: Item) {
  const payload = payloadOf(item);
  return item.summaryText || textValue(payload.message ?? payload.content ?? payload.text ?? payload.summary);
}

function shorten(value: string, max: number) {
  return value.length > max ? `${value.slice(0, max - 1).trimEnd()}…` : value;
}

function cleanPreviewLine(value: string) {
  return value.replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/^[\s#>*`-]+/, '').replace(/[*_`]+/g, '').replace(/\s+/g, ' ').trim();
}

function shortThreadId(value: string) {
  return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value;
}

export function threadDisplay(thread: Thread): { title: string; excerpt: string } {
  if (thread.name?.trim()) return { title: shorten(cleanPreviewLine(thread.name), 52), excerpt: '' };
  if (thread.agentNickname?.trim()) return { title: shorten(cleanPreviewLine(thread.agentNickname), 52), excerpt: '子代理会话' };
  const preview = (thread.lastMessagePreview || '').trim();
  const outcome = preview.match(/"outcome"\s*:\s*"([^"]+)/i)?.[1];
  const rationale = preview.match(/"rationale"\s*:\s*"([^"]+)/i)?.[1];
  if (outcome || /"risk_level"\s*:/.test(preview)) {
    const outcomeLabel = ({ allow: '允许', allowed: '允许', deny: '拒绝', denied: '拒绝', review: '需复核' } as Record<string, string>)[outcome?.toLowerCase() || ''];
    return { title: `审批审查${outcomeLabel ? ` · ${outcomeLabel}` : ''}`, excerpt: rationale ? shorten(cleanPreviewLine(rationale), 68) : '自动审批子代理记录' };
  }
  if (/^<turn_aborted>/i.test(preview)) return { title: '已中断的会话', excerpt: '执行被中断，历史记录可能不完整' };
  if (/^call_[\w-]+$/i.test(preview) || /^(exec|tool_output)$/i.test(preview)) return { title: '工具调用记录', excerpt: '' };
  if (/^The following is the Codex agent history whose request action you are assessing/i.test(preview)) {
    return { title: '审批审查记录', excerpt: '自动审批子代理记录' };
  }
  if (/^\*\*(Planning|Designing|Considering|Reviewing|Inspecting|Implementing|Running|Checking|Preparing|Analyzing|Exploring|Refining|Verifying)/i.test(preview)) {
    return { title: thread.status === 'active' ? '当前会话 · 正在处理' : '处理过程记录', excerpt: '' };
  }
  if (/^(<app-context>|<multi_agent_mode>|<environment_context>|<codex_internal_context|# AGENTS\.md instructions for)/i.test(preview)) {
    return { title: '会话上下文记录', excerpt: '' };
  }
  const lines = preview.split(/\r?\n/).map(cleanPreviewLine).filter(Boolean);
  const title = lines[0] ? shorten(lines[0], 52) : `会话 ${shortThreadId(thread.codexThreadId)}`;
  const excerpt = lines.slice(1).find((line) => line !== title) || '';
  return { title, excerpt: shorten(excerpt, 68) };
}

function itemBlobRefs(item: Item): { blobId: string; size: number }[] {
  const refs = record(item.raw).blobRefs;
  if (!Array.isArray(refs)) return [];
  return refs.map(record).map((ref) => ({ blobId: String(ref.blobId || ''), size: Number(ref.size || 0) })).filter((ref) => ref.blobId);
}

export function itemRendererKind(itemType: string) {
  const presentation = presentItem({ turnScope: '', itemId: '', itemType, status: '', raw: {}, provenance: {}, lastEventSeq: 0 });
  if (presentation.group === 'dialogue') return 'message';
  if (presentation.group === 'request') return 'request';
  if (presentation.group === 'status') return 'status';
  if (presentation.group === 'unknown') return 'unknown';
  if (presentation.activity === 'collaboration') return 'collaboration';
  if (presentation.activity === 'search' || presentation.activity === 'image') return 'media';
  return 'tool';
}

function isAbort(error: unknown) { return error instanceof DOMException && error.name === 'AbortError'; }
function requestMessage(error: unknown) { return error instanceof Error ? error.message : '请求失败'; }

export function selectThreadControlCatalog(catalogs: ControlCatalog[]) {
  return catalogs.find((catalog) => catalog.threadLoaded)
    || catalogs.find((catalog) => catalog.slashCommands.some((command) => command.capability === 'thread.resume'));
}

function settingId(value: unknown) {
  if (typeof value === 'string') return value || null;
  const entry = record(value);
  const id = entry.id ?? entry.name ?? entry.value;
  return typeof id === 'string' && id ? id : null;
}

export function clearThreadInput(thread: Thread, turns: Turn[], catalog: ControlCatalog): NewThreadInput | undefined {
  const cwd = thread.context.runtime.cwd || thread.cwdDisplay;
  if (!cwd?.startsWith('/')) return undefined;
  const latestContext = [...turns].reverse().find((turn) => turn.context)?.context;
  return {
    sourceId: catalog.sourceId,
    sourceEpoch: catalog.sourceEpoch,
    cwd,
    model: thread.context.runtime.model || thread.model || latestContext?.model || null,
    personality: settingId(latestContext?.personality),
    permissions: settingId(thread.context.runtime.activePermissionProfile || latestContext?.permissionProfile)
  };
}

export function deferInitialDashboardForPairing(hash: string) {
  return Boolean(new URLSearchParams(hash.slice(1)).get('pair'));
}

export function shouldFollowLatestMessage(distanceFromBottom: number) {
  return distanceFromBottom <= 120;
}

export function canNavigateComposerHistory(value: string, selectionStart: number, selectionEnd: number, direction: 'previous' | 'next') {
  if (selectionStart !== selectionEnd) return false;
  return direction === 'previous' ? !value.slice(0, selectionStart).includes('\n') : !value.slice(selectionEnd).includes('\n');
}

export function resizeComposerTextarea(textarea: HTMLTextAreaElement) {
  textarea.style.height = 'auto';
  const height = Math.min(220, Math.max(64, textarea.scrollHeight));
  textarea.style.height = `${height}px`;
  textarea.style.overflowY = textarea.scrollHeight > 220 ? 'auto' : 'hidden';
}

export function currentSessionPresence(thread: Thread, catalog?: ControlCatalog, controlError?: string) {
  if (thread.archived) return { label:'已归档', tone:'muted', detail:'该会话当前仅供浏览' };
  if (catalog?.activeTurnId) return { label:'正在处理', tone:'live', detail:'Codex 正在执行当前 Turn' };
  if (!controlError && catalog?.threadLoaded) return { label:'可继续', tone:'ready', detail:'已连接到当前 App Server source' };
  if (controlError) return { label:'仅浏览', tone:'warning', detail:controlError };
  if (thread.stale || thread.status === 'active') return { label:'状态待同步', tone:'warning', detail:'尚未确认实时控制状态' };
  return { label:'历史会话', tone:'muted', detail:'未加载到可控 App Server source' };
}

export function commandNotice(command: GatewayCommand) {
  const state = ({
    received: '已接收',
    authorized: '已授权',
    dispatching: '正在派发',
    accepted_by_source: '已由 Source 接受',
    running: '正在运行',
    completed: '已完成',
    rejected: '已拒绝',
    failed: '失败',
    cancelled: '已取消'
  } as Record<string, string>)[command.state] || command.state.replaceAll('_', ' ');
  if (command.state === 'completed' && command.capability === 'thread.goal.get') {
    const result = record(command.result);
    return result.goalPresent === false
      ? '当前会话没有 Goal'
      : `当前 Goal：${String(result.goalStatus || 'active')}`;
  }
  if (command.state === 'completed' && command.capability === 'thread.goal.clear') return 'Goal 已清除';
  if (command.state === 'completed' && command.capability === 'thread.goal.set') return 'Goal 状态已更新';
  return command.state === 'outcome_unknown'
    ? '操作结果未知：请刷新状态，系统不会自动重放'
    : `控制操作${state}`;
}

export function FeedbackNotice({ message, onClose }: { message: string; onClose: () => void }) {
  const tone = message.includes('结果未知') || message.includes('不会自动重放') ? 'warning'
    : /失败|错误|拒绝|取消|断开/.test(message) ? 'error'
      : /已完成|已刷新|已复制/.test(message) ? 'success' : 'info';
  return <div class={`notice notice-${tone}`} role={tone === 'error' ? 'alert' : 'status'}>
    <span>{message}</span><button type="button" onClick={onClose} aria-label="关闭操作提示">关闭</button>
  </div>;
}

function ErrorNotice({ message, onRetry, onClose }: { message: string; onRetry?: () => void; onClose: () => void }) {
  return <div class="notice notice-error" role="alert"><span>{message}</span><span class="notice-actions">
    {onRetry && <button type="button" onClick={onRetry}>重试</button>}
    <button type="button" onClick={onClose} aria-label="关闭错误提示">关闭</button>
  </span></div>;
}

function AuthPanel({ onConnect, error, checking }: { onConnect: (token: string) => void; error: string; checking: boolean }) {
  const [value, setValue] = useState('');
  return <section class="auth-panel" aria-busy={checking}><p class="eyebrow">LOCAL · READ ONLY</p>
    <h2>{tailscaleViewer ? 'Tailscale 私网访问未授权' : '使用本机访问链接'}</h2><p class="auth-guidance">{tailscaleViewer
      ? '请确认当前设备已登录同一 Tailnet，并且通过 Observer 启动输出中的 Tailscale Viewer 地址访问。'
      : '请打开 Observer 启动输出中的单次配对链接。链接会安全地换成本机 Cookie，地址栏不会保留访问凭据。'}</p>
    {error && <p class="error" role="alert">{error}</p>}
    <details class="advanced-auth"><summary>高级访问：使用 Bearer token</summary>
      <form onSubmit={(event) => { event.preventDefault(); onConnect(value.trim()); }}>
        <label for="token">Bearer token（仅用于故障恢复或 API 调试）</label><div class="auth-row">
          <input id="token" type="password" autocomplete="off" value={value}
            onInput={(event) => setValue((event.target as HTMLInputElement).value)} placeholder="粘贴本机 token" />
          <button type="submit" disabled={!value.trim() || checking}>{checking ? '连接中…' : '连接'}</button>
        </div>
      </form>
    </details>
  </section>;
}

function ContextPanel({ thread }: { thread: Thread }) {
  const { session, runtime } = thread.context;
  const instructions = record(session.baseInstructions);
  const instructionText = typeof session.baseInstructions === 'string' ? session.baseInstructions
    : typeof instructions.text === 'string' ? instructions.text : '';
  return <details class="context-panel"><summary>背景上下文</summary><div class="context-grid">
    <div class="context-card"><h4>Session Instructions</h4>{instructionText
      ? <div class="markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(instructionText) }} />
      : <p class="thread-meta">未记录</p>}</div>
    <div class="context-card"><h4>Session Metadata</h4><pre>{jsonText({
      agentNickname: session.agentNickname, agentRole: session.agentRole, agentPath: session.agentPath,
      originator: session.originator, cliVersion: session.cliVersion, threadSource: session.threadSource,
      historyMode: session.historyMode, historyBase: session.historyBase, modelProvider: session.modelProvider,
      dynamicTools: session.dynamicTools, selectedCapabilityRoots: session.selectedCapabilityRoots,
      memoryMode: session.memoryMode, multiAgentVersion: session.multiAgentVersion, contextWindow: session.contextWindow
    })}</pre></div>
    <div class="context-card"><h4>Runtime Context</h4><pre>{jsonText(runtime)}</pre></div>
  </div></details>;
}

function SummaryFields({ item }: { item: Item }) {
  const payload = payloadOf(item);
  const kind = itemRendererKind(item.itemType);
  const candidates = kind === 'tool' ? ['command', 'cmd', 'name', 'tool', 'server', 'path', 'call_id', 'exit_code', 'duration_ms']
    : kind === 'collaboration' ? ['agent', 'agent_id', 'recipient', 'sender', 'task', 'status']
      : kind === 'media' ? ['query', 'url', 'path', 'mime_type', 'size', 'width', 'height']
        : kind === 'request' ? ['request_id', 'request_type', 'state', 'source_id']
          : ['type', 'status', 'message', 'usage', 'duration_ms'];
  const values = candidates.filter((key) => payload[key] != null).map((key) => [key.replaceAll('_', ' '), textValue(payload[key])]);
  if (!values.length) return null;
  return <dl class="item-fields">{values.map(([key, value]) => <><dt>{key}</dt><dd>{value}</dd></>)}</dl>;
}

export function ItemCard({ item, token, onBlob }: { item: Item; token: string; onBlob: (message: string) => void }) {
  const presentation = presentItem(item);
  const summary = itemSummary(item);
  const phase = String(payloadOf(item).phase || record(item.raw).phase || '');
  const refs = itemBlobRefs(item);
  const itemDuration = duration(item.startedAtMs, item.completedAtMs);
  return <div class={`activity-record activity-${presentation.activity || 'other'}`} id={`item-${encodeURIComponent(item.itemId)}`} data-item-id={item.itemId}>
    <div class="activity-record-heading"><span>{item.itemType.replaceAll('_', ' ')}</span>
      <span>{phase && `${phase.replaceAll('_', ' ')} · `}{item.status}</span></div>
    <div class="activity-meta"><span>{formatTime(item.startedAtMs || item.completedAtMs)}</span>{itemDuration && <span>持续 {itemDuration}</span>}</div>
    {presentation.group !== 'unknown' && <SummaryFields item={item} />}
    {summary && <p class="activity-summary">{summary}</p>}
    {presentation.group === 'request' && <p class="readonly-notice">Observer V1 为只读模式，请在原 Codex 客户端中处理该请求。</p>}
    {presentation.group === 'unknown' && <div class="unknown-renderer"><p>未知 Item 类型；原始投影已保留在诊断中。</p><dl class="item-fields">
      <dt>method</dt><dd>{String(payloadOf(item).method || record(item.raw).type || 'unknown')}</dd>
      <dt>phase</dt><dd>{phase || 'unknown'}</dd><dt>size</dt><dd>{new Blob([jsonText(item.raw)]).size} bytes</dd>
    </dl><pre>{jsonText(item.raw)}</pre></div>}
    {refs.map((ref) => <button class="blob-download" type="button" onClick={() => downloadBlob(ref.blobId, token, onBlob)}>
      下载已脱敏大 payload（{ref.size} bytes）</button>)}
    <details class="item-raw"><summary>已脱敏 JSON / provenance</summary><pre>{jsonText({ raw: item.raw, provenance: item.provenance })}</pre></details>
  </div>;
}

function DialogueMessage({ item }: { item: Item }) {
  const presentation = presentItem(item);
  const summary = dialogueMessageText(item);
  const imageCount = dialogueImageCount(item);
  const phase = String(payloadOf(item).phase || '');
  const assistant = presentation.role !== 'user';
  return <article class={`dialogue-message dialogue-${presentation.role || 'assistant'}`} id={`item-${encodeURIComponent(item.itemId)}`}
    data-item-id={item.itemId}>
    {assistant && <span class="dialogue-avatar"><Icon name="codex" size={15} /></span>}
    <div class="dialogue-body"><header><strong>{presentation.label}</strong><span>{formatClock(item.startedAtMs || item.completedAtMs)}{phase ? ` · ${phaseLabel(phase)}` : ''}</span></header>
      {summary ? <div class="dialogue-content markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(summary) }} />
        : <p class="empty-message">消息正文未保留</p>}
      {imageCount > 0 && <p class="dialogue-attachments">图片附件 · {imageCount} 张</p>}
    </div>
  </article>;
}

export interface OptimisticMessage {
  threadKey: string;
  clientUserMessageId: string;
  text: string;
  imageCount: number;
  createdAtMs: number;
  state: 'sending' | 'accepted' | 'outcome_unknown' | 'failed';
  error?: string;
}

function projectedClientMessageIds(items: Item[]) {
  const ids = new Set<string>();
  for (const item of items) {
    if (item.itemType !== 'user_message') continue;
    const raw = record(item.raw); const payload = payloadOf(item); const provenance = record(item.provenance);
    for (const candidate of [item.itemId, raw.clientId, raw.client_id, raw.clientUserMessageId, payload.clientId, payload.client_id,
      payload.clientUserMessageId, provenance.clientUserMessageId]) {
      if (typeof candidate === 'string' && candidate) ids.add(candidate);
    }
  }
  return ids;
}

function rolloutMessageVariant(item: Item) {
  const raw = record(item.raw);
  if ((item.itemType === 'user_message' && raw.type === 'user_message')
    || (item.itemType === 'agent_message' && raw.type === 'agent_message')) return 'event';
  if (raw.type === 'message' && ((item.itemType === 'user_message' && raw.role === 'user')
    || (item.itemType === 'agent_message' && raw.role === 'assistant'))) return 'response';
  return undefined;
}

function appServerMessageVariant(item: Item) {
  const raw = record(item.raw);
  return ((item.itemType === 'user_message' && raw.type === 'userMessage')
    || (item.itemType === 'agent_message' && raw.type === 'agentMessage')) ? 'live' : undefined;
}

function dialogueDedupeKey(item: Item) {
  return `${item.turnScope}\0${item.itemType}\0${dialogueMessageText(item)}`;
}

function dialogueMessageText(item: Item) {
  return itemSummary(item)
    .replace(/<image\b[^>]*>[\s\S]*?<\/image>/gi, '')
    .replace(/<image\b[^>]*\/?\s*>/gi, '')
    .trim();
}

function dialogueImageCount(item: Item) {
  const raw = record(item.raw);
  const content = Array.isArray(raw.content) ? raw.content.map(record) : [];
  const inline = content.filter((entry) => ['localImage','input_image','image'].includes(String(entry.type || ''))).length;
  const local = Array.isArray(raw.local_images) ? raw.local_images.length
    : Array.isArray(raw.localImages) ? raw.localImages.length : 0;
  return Math.max(inline, local);
}

export function dedupeDialogueItems(items: Item[]) {
  const eventBuckets = new Map<string, Item[]>();
  const rolloutBuckets = new Map<string, Item[]>();
  for (const item of items) {
    const variant = rolloutMessageVariant(item);
    if (!variant) continue;
    const key = dialogueDedupeKey(item);
    rolloutBuckets.set(key, [...(rolloutBuckets.get(key) || []), item]);
    if (variant === 'event') eventBuckets.set(key, [...(eventBuckets.get(key) || []), item]);
  }
  const matchedEvents = new Set<Item>();
  const mirroredResponses = new Set<Item>();
  for (const item of items) {
    if (rolloutMessageVariant(item) !== 'response') continue;
    const key = dialogueDedupeKey(item);
    const match = eventBuckets.get(key)?.find((candidate) => {
      if (matchedEvents.has(candidate)) return false;
      return true;
    });
    if (match) { matchedEvents.add(match); mirroredResponses.add(item); }
  }
  const matchedRollout = new Set<Item>();
  const mirroredLiveItems = new Set<Item>();
  for (const item of items) {
    if (appServerMessageVariant(item) !== 'live') continue;
    const match = rolloutBuckets.get(dialogueDedupeKey(item))?.find((candidate) => {
      if (matchedRollout.has(candidate)) return false;
      return true;
    });
    if (match) { matchedRollout.add(match); mirroredLiveItems.add(item); }
  }
  return items.filter((item) => !mirroredResponses.has(item) && !mirroredLiveItems.has(item));
}

export function reconcileOptimisticMessages(messages: OptimisticMessage[], threadKey: string, items: Item[]) {
  const projected = projectedClientMessageIds(items);
  return messages.filter((message) => message.threadKey !== threadKey || !projected.has(message.clientUserMessageId));
}

export function OptimisticMessageCard({ message }: { message: OptimisticMessage }) {
  const status = ({ sending:'正在发送', accepted:'已提交，等待投影', outcome_unknown:'结果未知，不会自动重放', failed:'发送失败' } as const)[message.state];
  const content = message.text || (message.imageCount ? '图片消息' : '空消息');
  return <article class={`dialogue-message dialogue-user optimistic-message optimistic-${message.state}`}
    data-client-user-message-id={message.clientUserMessageId} role="status">
    <div class="dialogue-body"><header><strong>你</strong><span>{formatClock(message.createdAtMs)} · {status}</span></header>
      <div class="dialogue-content markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }} />
      {message.imageCount > 0 && <p class="optimistic-images">{message.imageCount} 张本地图片</p>}
      {message.error && <p class="optimistic-error">{message.error}</p>}
    </div>
  </article>;
}

function ActivityPanel({ entries, token, onBlob, focusedItemId }: {
  entries: ActivityEntry[]; token: string; onBlob: (message: string) => void; focusedItemId?: string;
}) {
  if (entries.length === 0) return null;
  const attention = entries.some((entry) => entry.presentation.group === 'request'
    || entry.presentation.activity === 'error' || entry.presentation.activity === 'interrupt'
    || ['failed', 'error', 'pending', 'request', 'waiting'].includes(entry.status));
  const focused = focusedItemId && entries.some((entry) => entry.items.some((item) => item.itemId === focusedItemId));
  return <details class={`activity-panel${attention ? ' activity-attention' : ''}`} open={Boolean(attention || focused)}>
    <summary><span>过程与诊断</span><span class="activity-counts">{summarizeActivities(entries)}</span></summary>
    <div class="activity-list">{entries.map((entry) => <section class={`activity-entry activity-entry-${entry.presentation.activity || 'other'}`}>
      <div class="activity-entry-heading"><strong>{entry.presentation.label}</strong>{badge(entry.status, `${entry.presentation.label}状态`)}</div>
      {entry.items.map((item) => <ItemCard key={`${item.turnScope}:${item.itemId}`} item={item} token={token} onBlob={onBlob} />)}
    </section>)}</div>
  </details>;
}

export function TurnSection({ turn, items, token, onBlob, focusedItemId, ordinal }: {
  turn: Turn | null; items: Item[]; token: string; onBlob: (message: string) => void; focusedItemId?: string; ordinal: number;
}) {
  const turnDuration = turn ? duration(turn.startedAtMs, turn.completedAtMs) : undefined;
  const dialogue = dedupeDialogueItems(items.filter((item) => presentItem(item).group === 'dialogue'));
  const activities = coalesceActivities(items);
  return <section class="turn" id={turn ? `turn-${encodeURIComponent(turn.turnId)}` : undefined}>
    <div class="turn-heading"><div><h3>{turn ? `第 ${ordinal} 轮` : '未归属记录'}</h3>
      {turn && <p class="turn-meta">{formatTime(turn.startedAtMs || turn.completedAtMs)}{turnDuration ? ` · ${turnDuration}` : ''} · {statusLabel(turn.status)}</p>}</div>
      {turn && turn.captureCompleteness !== 'durable_complete' && badge(turn.captureCompleteness, 'Turn 捕获完整性')}</div>
    {turn?.completenessReasons.length ? <p class="completeness-reasons">{turn.completenessReasons.join(' · ')}</p> : null}
    {dialogue.map((item) => <DialogueMessage key={`${item.turnScope}:${item.itemId}`} item={item} />)}
    {dialogue.length === 0 && <p class="empty-dialogue">这一轮没有保留可展示的对话正文。</p>}
    <ActivityPanel entries={activities} token={token} onBlob={onBlob} focusedItemId={focusedItemId} />
  </section>;
}

function RelationLink({ label, relation, onNavigate }: { label: string; relation: NonNullable<ThreadDetail['relations']['parent']>; onNavigate: (key: string) => void }) {
  const title = relation.name || relation.codexThreadId || relation.threadKey;
  return relation.resolved
    ? <button class="relation-link" type="button" onClick={() => onNavigate(relation.threadKey)}>{label}: {title}</button>
    : <span class="relation-unresolved">{label}: {title.slice(0, 24)} · 未解析</span>;
}

function DiagnosticsSummary({ detail, health }: { detail: ThreadDetail; health?: Health }) {
  const legacy = health?.privacy?.legacyRedactionEvents || 0;
  const issueCount = detail.diagnostics.decodeErrors + detail.diagnostics.unknownVariants + detail.pendingRequests.length
    + detail.projectionConflicts.length + (legacy > 0 ? 1 : 0);
  return <details class={`diagnostic-summary${issueCount ? ' diagnostic-warning' : ''}`} aria-label="会话诊断摘要">
    <summary><span>会话诊断</span><span>{issueCount ? `${issueCount} 项需要关注` : '未发现异常'}</span></summary><div class="diagnostic-content"><div class="diagnostic-grid">
    <div><strong>{detail.diagnostics.decodeErrors}</strong><span>解析错误</span></div>
    <div><strong>{detail.diagnostics.unknownVariants}</strong><span>未知变体</span></div>
    <div><strong>{detail.pendingRequests.length}</strong><span>待处理请求</span></div>
    <div><strong>{detail.projectionConflicts.length}</strong><span>投影冲突</span></div>
  </div>{legacy > 0 && <p class="privacy-warning" role="status">隐私提醒：{health?.privacy?.warning || `${legacy} 条记录使用旧版脱敏规则`}</p>}
    <details><summary>覆盖证据</summary><pre>{jsonText(detail.coverageSummary)}</pre></details>
    {detail.pendingRequests.map((request) => <div class="pending-request"><strong>{request.requestType || 'request'} · {request.state || 'pending'}</strong>
      <span>{request.sourceId || 'unknown source'} · epoch {request.sourceEpoch || 'unknown'}</span></div>)}
    {detail.projectionConflicts.length > 0 && <details><summary>查看投影冲突</summary><pre>{jsonText(detail.projectionConflicts)}</pre></details>}</div>
  </details>;
}

export function GatewayStatusCardView({ card }: { card: GatewayStatusCard }) {
  return <section class="command-status gateway-status-card" role="status" aria-label={`Gateway ${card.cardType} status card`}>
    <strong>Gateway /{card.cardType}</strong><span>{card.sourceId.slice(0, 8)} · epoch {card.sourceEpoch.slice(0, 8)}</span>
    <details><summary>查看状态</summary><pre>{jsonText(card.content)}</pre></details>
  </section>;
}

function isGatewayStatusCard(value: GatewayCommand | GatewayStatusCard): value is GatewayStatusCard {
  return 'kind' in value && value.kind === 'gatewayStatusCard';
}

function requestPayloadSummary(request: PendingRequest) {
  const payload = record(request.payload);
  const fields = ['command', 'cwd', 'grantRoot', 'reason', 'permissions', 'questions', 'serverName', 'message', 'requestedSchema'];
  return Object.fromEntries(fields.filter((field) => payload[field] != null).map((field) => [field, payload[field]]));
}

function ActiveTurnIndicator({ command }: { command?: GatewayCommand }) {
  const steering = command?.capability === 'turn.steer' && ['received','authorized','dispatching','accepted_by_source','running'].includes(command.state);
  return <div class="active-turn-indicator" role="status" aria-live="polite"><span class="thinking-pulse"><i /><i /><i /></span>
    <span><strong>{steering ? 'Steering' : 'Thinking'}</strong>{steering ? ' · 正在追加你的指令' : ' · Codex 正在处理'}</span></div>;
}

export function ConversationFollowButton({ unseenUpdates, onJump }: { unseenUpdates: number; onJump: () => void }) {
  return <button class="jump-to-latest" type="button" onClick={onJump} aria-label="回到最新消息">
    <Icon name="down" size={14} /><span>{unseenUpdates > 0 ? `回到最新 · ${unseenUpdates} 条新进展` : '回到最新'}</span>
  </button>;
}

function CurrentSessionOverview({ thread, turnCount, pendingCount }: {
  thread: Thread; turnCount: number; pendingCount: number;
}) {
  const presence = thread.archived
    ? { label:'已归档', tone:'muted', detail:'该会话当前仅供浏览' }
    : { label:'History 只读', tone:'muted', detail:'活动输入由终端 Session Runtime 承载' };
  return <div class="session-overview" aria-label="当前会话状态">
    <span class="session-kicker">{thread.archived ? '归档会话' : '当前会话'}</span>
    <span class={`session-presence session-presence-${presence.tone}`} title={presence.detail}><i />{presence.label}</span>
    <span>{turnCount} 轮对话</span>
    {pendingCount > 0 && <span class="session-attention">{pendingCount} 个请求待处理</span>}
    {thread.captureCompleteness !== 'durable_complete' && <span class="session-attention">{completenessLabel(thread.captureCompleteness)}</span>}
  </div>;
}

export function PendingRequestCard({ request, busy, onAction }: {
  request: PendingRequest; busy: boolean; onAction: (request: PendingRequest, action: Record<string, unknown>) => void;
}) {
  const [answer, setAnswer] = useState('');
  const actionable = request.state === 'pending' && !busy;
  const questions = Array.isArray(request.payload.questions) ? request.payload.questions.map(record) : [];
  const formSchema = record(request.payload.requestedSchema); const formProperties = record(formSchema.properties);
  const approval = request.requestType === 'approval';
  const permissionRequest = approval && request.payload.permissions != null;
  return <article class={`request-card request-${request.state}`} aria-label={`${request.requestType} request`}>
    <header><strong>{request.requestType?.replaceAll('_', ' ') || 'request'}</strong>{badge(request.state)}</header>
    <pre>{jsonText(requestPayloadSummary(request))}</pre>
    {request.requestType === 'user_input' && questions.length > 0 && <label>回答（每行对应一个问题）
      <textarea value={answer} onInput={(event) => setAnswer((event.target as HTMLTextAreaElement).value)} disabled={!actionable} /></label>}
    {request.requestType === 'mcp_elicitation' && Object.keys(formProperties).length > 0 && <label>表单 JSON
      <textarea value={answer} placeholder="{}" onInput={(event) => setAnswer((event.target as HTMLTextAreaElement).value)} disabled={!actionable} /></label>}
    <div class="request-actions">
      {approval && !permissionRequest && <><button type="button" disabled={!actionable} onClick={() => onAction(request, { type: 'approval', decision: 'accept' })}>允许</button>
        <button type="button" disabled={!actionable} onClick={() => onAction(request, { type: 'approval', decision: 'acceptForSession' })}>本次会话允许</button>
        <button type="button" disabled={!actionable} onClick={() => onAction(request, { type: 'approval', decision: 'decline' })}>拒绝</button></>}
      {permissionRequest && <><button type="button" disabled={!actionable} onClick={() => onAction(request,
        { type: 'permissions', grant: true, scope: 'turn', strictAutoReview: null })}>允许本 Turn</button>
        <button type="button" disabled={!actionable} onClick={() => onAction(request,
          { type: 'permissions', grant: false, scope: 'turn', strictAutoReview: null })}>拒绝</button></>}
      {request.requestType === 'user_input' && <button type="button" disabled={!actionable || !answer.trim()} onClick={() => {
        const lines = answer.split(/\r?\n/); const answers = Object.fromEntries(questions.map((question, index) => [String(question.id), [lines[index] || '']]));
        onAction(request, { type: 'userInput', answers });
      }}>提交回答</button>}
      {request.requestType === 'mcp_elicitation' && <><button type="button" disabled={!actionable || !answer.trim()} onClick={() => {
        try { onAction(request, { type: 'mcpElicitation', action: 'accept', content: JSON.parse(answer) }); } catch { /* server is never called for malformed local JSON */ }
      }}>提交表单</button><button type="button" disabled={!actionable} onClick={() => onAction(request, { type: 'mcpElicitation', action: 'decline' })}>拒绝</button></>}
    </div>
  </article>;
}

function slashCommandDescription(command: ControlCatalog['slashCommands'][number]) {
  return ({
    '/new':'新建对话', '/clear':'清空界面并开始新对话', '/resume':'加载当前会话', '/fork':'创建分支', '/rename':'重命名会话', '/archive':'归档会话',
    '/compact':'压缩上下文', '/review':'开始代码审查', '/interrupt':'停止当前 Turn', '/plan':'切换 Plan 模式',
    '/goal':'查看或设置 Goal', '/model':'选择模型', '/reasoning':'选择推理强度', '/personality':'选择个性',
    '/permissions':'选择权限', '/mcp':'查看 MCP 状态', '/status':'查看 Gateway 状态', '/usage':'查看用量'
  } as Record<string, string>)[command.name] || command.capability;
}

export function Composer({ catalog, thread, disabledReason, busy, onSend, onInterrupt, onSetting, onSlash }: {
  catalog?: ControlCatalog; thread?: Thread; disabledReason?: string; busy: boolean;
  onSend: (text: string, images: File[]) => void | boolean | Promise<void | boolean>; onInterrupt: () => void;
  onSetting?: (capability: string, value: string) => void; onSlash?: (command: string) => void;
}) {
  const [text, setText] = useState(''); const [images, setImages] = useState<File[]>([]);
  const [activeOption, setActiveOption] = useState(0); const [paletteClosed, setPaletteClosed] = useState(false);
  const [history, setHistory] = useState<string[]>([]); const [historyIndex, setHistoryIndex] = useState<number | null>(null);
  const [historyDraft, setHistoryDraft] = useState('');
  const [submitting, setSubmitting] = useState(false); const textareaRef = useRef<HTMLTextAreaElement>(null);
  const locked = Boolean(disabledReason); const disabled = locked || busy || submitting;
  const activeTurn = Boolean(catalog?.activeTurnId);
  const resumable = Boolean(catalog && !catalog.threadLoaded
    && catalog.slashCommands.some((command) => command.capability === 'thread.resume'));
  const picker = catalog && !paletteClosed ? slashPicker(text, catalog, thread) : undefined;
  const hasInput = Boolean(text.trim() || images.length);
  const slashToken = text.trim().split(/\s/, 1)[0];
  const editingSlashName = text.startsWith('/') && !text.startsWith('//') && !/\s/.test(text);
  const palette = !picker && !paletteClosed && editingSlashName
    ? (catalog?.slashCommands.filter((command) => command.name.startsWith(slashToken)) || []) : [];
  const optionCount = picker?.options.length || palette.length;

  useLayoutEffect(() => { if (textareaRef.current) resizeComposerTextarea(textareaRef.current); }, [text]);

  function updateText(value: string) {
    setText(value); setPaletteClosed(false); setActiveOption(0); setHistoryIndex(null);
  }

  function settleSubmission(accepted: void | boolean, submittedText: string) {
    if (accepted === false) { textareaRef.current?.focus(); return false; }
    const historyText = submittedText.trim();
    if (historyText) setHistory((current) => [...current.filter((entry) => entry !== historyText), historyText].slice(-50));
    setText(''); setImages([]); setPaletteClosed(false); setActiveOption(0); textareaRef.current?.focus();
    setHistoryIndex(null); setHistoryDraft('');
    return true;
  }

  function submit(nextText = text, nextImages = images): boolean | Promise<boolean> {
    if (disabled || (!nextText.trim() && nextImages.length === 0)) return false;
    try {
      const result = onSend(nextText, nextImages);
      if (!result || typeof (result as Promise<void | boolean>).then !== 'function') return settleSubmission(result as void | boolean, nextText);
      setSubmitting(true);
      return Promise.resolve(result).then((accepted) => settleSubmission(accepted, nextText)).catch(() => { textareaRef.current?.focus(); return false; })
        .finally(() => setSubmitting(false));
    } catch {
      textareaRef.current?.focus();
      return false;
    }
  }

  function chooseCommand(command: ControlCatalog['slashCommands'][number]) {
    const exact = text.trim() === command.name;
    if (exact && !command.interactionRequiredWithoutArgument) void submit(command.name, []);
    else {
      updateText(command.name);
      requestAnimationFrame(() => textareaRef.current?.focus());
    }
  }

  function choosePicker(index: number) {
    if (!picker) return;
    const option = picker.options[index];
    if (option) void submit(`${picker.command} ${option.value}`, []);
  }

  function navigateHistory(direction: 'previous' | 'next') {
    if (history.length === 0) return;
    let nextIndex: number | null = historyIndex;
    let nextText = text;
    if (direction === 'previous') {
      if (historyIndex == null) { setHistoryDraft(text); nextIndex = history.length - 1; }
      else nextIndex = Math.max(0, historyIndex - 1);
      nextText = history[nextIndex];
    } else if (historyIndex != null && historyIndex < history.length - 1) {
      nextIndex = historyIndex + 1; nextText = history[nextIndex];
    } else {
      nextIndex = null; nextText = historyDraft;
    }
    setHistoryIndex(nextIndex); setText(nextText); setPaletteClosed(true); setActiveOption(0);
    requestAnimationFrame(() => {
      const textarea = textareaRef.current;
      if (textarea) textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    });
  }

  function appendImages(files: File[]) {
    const accepted = files.filter((file) => ['image/png','image/jpeg','image/webp','image/gif'].includes(file.type));
    if (accepted.length) setImages((current) => [...current, ...accepted].slice(0, 4));
  }

  return <section class={`composer${activeTurn ? ' composer-active' : ''}`} aria-label="Codex 控制 Composer">
    <div class="composer-status" id="composer-status">
      <span>{disabledReason || (activeTurn ? '在当前 Turn 中追加指令' : 'Source 已连接，可以继续对话')}</span>
      {resumable && <button type="button" disabled={busy} onClick={() => void onSend('/resume', [])}>加载到当前 source</button>}
    </div>
    <textarea ref={textareaRef} value={text} disabled={disabled} rows={1} aria-describedby="composer-status"
      placeholder={activeTurn ? '继续引导当前任务…' : '询问 Codex，输入 / 查看命令'}
      aria-controls={optionCount ? 'composer-command-options' : undefined}
      aria-activedescendant={optionCount ? `composer-option-${Math.min(activeOption, optionCount - 1)}` : undefined}
      onInput={(event) => updateText((event.target as HTMLTextAreaElement).value)} onPaste={(event) => {
        const files = Array.from(event.clipboardData?.files || []);
        if (files.some((file) => file.type.startsWith('image/'))) { event.preventDefault(); appendImages(files); }
      }} onKeyDown={(event) => {
        if (event.isComposing || event.keyCode === 229) return;
        if (optionCount && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
          event.preventDefault(); const delta = event.key === 'ArrowDown' ? 1 : -1;
          setActiveOption((current) => (current + delta + optionCount) % optionCount); return;
        }
        if (optionCount && event.key === 'Escape') { event.preventDefault(); setPaletteClosed(true); return; }
        if (optionCount && (event.key === 'Enter' || event.key === 'Tab') && !event.shiftKey) {
          event.preventDefault(); if (picker) choosePicker(activeOption); else if (palette[activeOption]) chooseCommand(palette[activeOption]); return;
        }
        if (!optionCount && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
          const textarea = event.currentTarget as HTMLTextAreaElement; const direction = event.key === 'ArrowUp' ? 'previous' : 'next';
          if ((direction === 'previous' || historyIndex != null)
            && canNavigateComposerHistory(textarea.value, textarea.selectionStart, textarea.selectionEnd, direction)) {
            event.preventDefault(); navigateHistory(direction); return;
          }
        }
        if (event.key === 'Escape' && activeTurn && !locked && !busy && !submitting) { event.preventDefault(); onInterrupt(); return; }
        if (event.key === 'Enter' && !event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey && (text.trim() || images.length)) {
          event.preventDefault(); void submit();
        }
      }} />
    {palette.length > 0 && <div id="composer-command-options" class="slash-palette" role="listbox" aria-label="可用 Slash commands">
      {palette.map((command, index) => <button id={`composer-option-${index}`} type="button" role="option" aria-selected={index === activeOption}
        onMouseEnter={() => setActiveOption(index)} onClick={() => chooseCommand(command)}><strong>{command.name}</strong>
        <small>{slashCommandDescription(command)}</small></button>)}</div>}
    {picker && <div id="composer-command-options" class="slash-picker" role="listbox" aria-label={picker.label}><strong>{picker.label}</strong>
      <div>{picker.options.map((option, index) => <button id={`composer-option-${index}`} type="button" role="option"
        aria-selected={index === activeOption || option.selected} onMouseEnter={() => setActiveOption(index)} onClick={() => choosePicker(index)}>{option.label}</button>)}</div>
    </div>}
    {images.length > 0 && <div class="image-preview">{images.map((file, index) => <span><span>{file.name} · {(file.size / 1024 / 1024).toFixed(1)} MiB</span>
      <button type="button" aria-label={`移除图片 ${file.name}`} disabled={disabled} onClick={() => setImages((current) => current.filter((_, item) => item !== index))}>
        <Icon name="close" size={13} /></button></span>)}</div>}
    <div class="composer-footer"><label class="image-picker" aria-label="添加图片"><Icon name="plus" size={18} />
      <input type="file" accept="image/png,image/jpeg,image/webp,image/gif" multiple disabled={disabled}
        onChange={(event) => appendImages(Array.from((event.target as HTMLInputElement).files || []))} /></label>
      {catalog?.threadLoaded && thread && onSetting && <ControlSettings thread={thread} catalog={catalog} busy={busy || submitting}
        onSetting={onSetting} onSlash={onSlash || ((command) => { void onSend(command, []); })} />}
      <span class="composer-shortcut">{activeTurn ? (hasInput ? 'Enter 追加 · Esc 中断' : 'Esc 中断当前 Turn') : 'Enter 发送 · Shift/Alt+Enter 换行'}</span><div class="composer-actions">
        {activeTurn && <button class="composer-stop" type="button" aria-label="停止当前 Turn" title="停止当前 Turn" disabled={locked || busy || submitting}
          onClick={onInterrupt}><Icon name="stop" size={15} /></button>}
        {(!activeTurn || hasInput) && <button class="composer-send" type="button" aria-label={activeTurn ? '发送追加指令' : '发送消息'} title={activeTurn ? '发送追加指令' : '发送消息'}
          disabled={disabled || !hasInput} onClick={() => void submit()}>{submitting ? <span class="button-spinner" /> : <Icon name="send" size={17} />}</button>}
      </div></div>
  </section>;
}

function catalogItems(catalog: ControlCatalog, method: string) {
  const entry = catalog.capabilities?.entries?.[method];
  return entry?.available && Array.isArray(entry.data?.data)
    ? entry.data.data.filter((item) => item.hidden !== true && item.allowed !== false)
    : [];
}

function catalogItemId(item: Record<string, unknown>) { return typeof item.id === 'string' ? item.id : ''; }
function catalogItemLabel(item: Record<string, unknown>) {
  return String(item.displayName || item.name || item.label || item.id || 'unknown');
}

function slashPicker(text: string, catalog: ControlCatalog, thread?: Thread) {
  const command = text.trim();
  const currentModel = thread?.context.runtime.model || thread?.model || '';
  const models = catalogItems(catalog, 'model/list');
  const model = models.find((item) => catalogItemId(item) === currentModel);
  const definitions = {
    '/model': { label:'选择 Model', items:models, value:catalogItemId, itemLabel:catalogItemLabel, current:currentModel },
    '/reasoning': { label:'选择 Reasoning', items:Array.isArray(model?.supportedReasoningEfforts) ? model.supportedReasoningEfforts.map(record) : [],
      value:(item: Record<string, unknown>) => String(item.reasoningEffort || ''), itemLabel:(item: Record<string, unknown>) => String(item.label || item.reasoningEffort || ''),
      current:thread?.context.runtime.reasoningEffort || thread?.reasoningEffort || '' },
    '/personality': { label:'选择 Personality', items:model?.supportsPersonality === true
      ? [{id:'none',name:'默认'},{id:'friendly',name:'Friendly'},{id:'pragmatic',name:'Pragmatic'}] : [],
      value:catalogItemId, itemLabel:catalogItemLabel, current:'' },
    '/permissions': { label:'选择 Permissions', items:catalogItems(catalog, 'permissionProfile/list'), value:catalogItemId,
      itemLabel:catalogItemLabel, current:typeof thread?.context.runtime.activePermissionProfile === 'string'
        ? thread.context.runtime.activePermissionProfile : String(record(thread?.context.runtime.activePermissionProfile).id || '') }
  } as const;
  const definition = definitions[command as keyof typeof definitions];
  if (!definition || !catalog.slashCommands.some((entry) => entry.name === command) || definition.items.length === 0) return undefined;
  return { command, label:definition.label, options:definition.items.map((item) => ({
    value:definition.value(item), label:definition.itemLabel(item), selected:definition.value(item) === definition.current
  })).filter((option) => option.value) };
}

export function ControlSettings({ thread, catalog, busy, onSetting, onSlash }: {
  thread: Thread; catalog: ControlCatalog; busy: boolean;
  onSetting: (capability: string, value: string) => void; onSlash: (command: string) => void;
}) {
  const [goal, setGoal] = useState('');
  const models = catalogItems(catalog, 'model/list'); const permissions = catalogItems(catalog, 'permissionProfile/list');
  const currentModel = thread.context.runtime.model || thread.model || catalogItemId(models[0] || {});
  const model = models.find((item) => catalogItemId(item) === currentModel);
  const efforts = Array.isArray(model?.supportedReasoningEfforts) ? model.supportedReasoningEfforts.map(record) : [];
  const permission = record(thread.context.runtime.activePermissionProfile);
  const currentPermission = typeof thread.context.runtime.activePermissionProfile === 'string'
    ? thread.context.runtime.activePermissionProfile : String(permission.id || '');
  const currentModelLabel = catalogItemLabel(models.find((item) => catalogItemId(item) === currentModel) || { id:currentModel || 'Source 默认' });
  const currentEffort = thread.context.runtime.reasoningEffort || thread.reasoningEffort || '默认';
  const currentPermissionLabel = catalogItemLabel(permissions.find((item) => catalogItemId(item) === currentPermission)
    || { id:currentPermission || '默认权限' });
  const planMode = catalog.collaborationMode?.mode === 'plan'; const goalStatus = catalog.goal?.status;
  const planAvailable = catalog.slashCommands.some((command) => command.capability === 'thread.plan');
  return <details class="control-settings"><summary aria-label="打开 Codex 控制设置"><span class="permission-summary"><Icon name="settings" size={15} />
    {currentPermissionLabel}</span><span class="model-summary">{currentModelLabel} · {currentEffort}</span></summary>
    <section class="control-settings-panel" aria-label="Codex 控制设置"><div class="setting-row">
      <label>Model<select aria-label="Model" value={currentModel} disabled={busy || models.length === 0}
        onChange={(event) => onSetting('thread.settings.model', (event.target as HTMLSelectElement).value)}>
        {models.map((item) => <option value={catalogItemId(item)}>{catalogItemLabel(item)}</option>)}</select></label>
      <label>Reasoning<select aria-label="Reasoning" value={thread.context.runtime.reasoningEffort || thread.reasoningEffort || ''}
        disabled={busy || efforts.length === 0} onChange={(event) => onSetting('thread.settings.reasoning', (event.target as HTMLSelectElement).value)}>
        {!thread.context.runtime.reasoningEffort && !thread.reasoningEffort && <option value="">默认</option>}
        {efforts.map((item) => <option value={String(item.reasoningEffort || '')}>{String(item.label || item.reasoningEffort || '')}</option>)}</select></label>
      <label>Permissions<select aria-label="Permissions" value={currentPermission} disabled={busy || permissions.length === 0}
        onChange={(event) => onSetting('thread.settings.permissions', (event.target as HTMLSelectElement).value)}>
        {!currentPermission && <option value="">默认</option>}{permissions.map((item) => <option value={catalogItemId(item)}>{catalogItemLabel(item)}</option>)}</select></label>
      {model?.supportsPersonality === true && <label>Personality<select aria-label="Personality" disabled={busy}
        onChange={(event) => onSetting('thread.settings.personality', (event.target as HTMLSelectElement).value)}>
        <option value="none">默认</option><option value="friendly">Friendly</option><option value="pragmatic">Pragmatic</option></select></label>}
    </div>
    <div class="mode-row">{planAvailable && <><span>Plan：{planMode ? '已启用' : '未启用'}</span><button type="button" disabled={busy}
      onClick={() => onSlash(planMode ? '/plan off' : '/plan')}>{planMode ? '退出 Plan' : '进入 Plan'}</button></>}<span>Goal：{goalStatus || '未设置'}</span>
      <input aria-label="Goal objective" value={goal} placeholder="设置 Goal" disabled={busy}
        onInput={(event) => setGoal((event.target as HTMLInputElement).value)} />
      <button type="button" disabled={busy || !goal.trim()} onClick={() => { onSlash(`/goal ${goal}`); setGoal(''); }}>设置</button>
      {goalStatus === 'active' && <button type="button" disabled={busy} onClick={() => onSlash('/goal pause')}>暂停</button>}
      {goalStatus === 'paused' && <button type="button" disabled={busy} onClick={() => onSlash('/goal resume')}>恢复</button>}
      {goalStatus && <button type="button" disabled={busy} onClick={() => onSlash('/goal clear')}>清除</button>}
    </div>
  </section></details>;
}

export function GoalStatusBar({ catalog, busy, onSlash }: {
  catalog: ControlCatalog; busy: boolean; onSlash: (command: string) => void;
}) {
  const goal = catalog.goal;
  if (!goal) return null;
  const status = goal.status || 'active';
  return <section class={`goal-status goal-${status}`} role="status" aria-label="当前 Goal">
    <div><span class="goal-mark" /><span><strong>{goal.objective || 'Goal 已设置'}</strong>
      <small>{status === 'paused' ? '已暂停' : '进行中'}{goal.tokensUsed != null ? ` · ${goal.tokensUsed.toLocaleString()} tokens` : ''}
        {goal.timeUsedSeconds != null ? ` · ${goal.timeUsedSeconds}s` : ''}</small></span></div>
    <div>{status === 'active'
      ? <button type="button" disabled={busy} onClick={() => onSlash('/goal pause')}>暂停</button>
      : <button type="button" disabled={busy} onClick={() => onSlash('/goal resume')}>恢复</button>}
      <button type="button" disabled={busy} onClick={() => onSlash('/goal clear')}>清除</button></div>
  </section>;
}

export function NewThreadDialog({ sources, catalog, busy, error, onSource, onClose, onCreate }: {
  sources: ControllerSource[]; catalog?: ControlCatalog; busy: boolean; error?: string;
  onSource: (sourceId: string) => void; onClose: () => void;
  onCreate: (input: NewThreadInput) => void;
}) {
  const [cwd, setCwd] = useState(''); const [model, setModel] = useState(''); const [personality, setPersonality] = useState('');
  const [permissions, setPermissions] = useState(''); const source = sources.find((value) => value.sourceId === catalog?.sourceId) || sources[0];
  const models = catalog ? catalogItems(catalog, 'model/list') : []; const profiles = catalog ? catalogItems(catalog, 'permissionProfile/list') : [];
  return <div class="dialog-backdrop"><section class="new-thread-dialog" role="dialog" aria-modal="true" aria-label="新建 Codex Thread">
    <header><div><p class="eyebrow">V2 CONTROL</p><h2>新建对话</h2></div><button type="button" onClick={onClose} disabled={busy}>关闭</button></header>
    {error && <p class="dialog-error" role="alert">{error}</p>}
    <label>Live source<select aria-label="Live source" value={source?.sourceId || ''} disabled={busy || sources.length === 0}
      onChange={(event) => onSource((event.target as HTMLSelectElement).value)}>{sources.map((value) =>
        <option value={value.sourceId}>{value.sourceId} · epoch {value.sourceEpoch.slice(0, 8)}</option>)}</select></label>
    <label>本机绝对 cwd<input aria-label="cwd" value={cwd} placeholder="/Users/me/workspace/project" disabled={busy}
      onInput={(event) => setCwd((event.target as HTMLInputElement).value)} /></label>
    <div class="new-thread-options"><label>Model<select aria-label="New thread model" value={model} disabled={busy || models.length === 0}
      onChange={(event) => setModel((event.target as HTMLSelectElement).value)}><option value="">Source 默认</option>
      {models.map((item) => <option value={catalogItemId(item)}>{catalogItemLabel(item)}</option>)}</select></label>
      <label>Permissions<select aria-label="New thread permissions" value={permissions} disabled={busy || profiles.length === 0}
        onChange={(event) => setPermissions((event.target as HTMLSelectElement).value)}><option value="">Source 默认</option>
        {profiles.map((item) => <option value={catalogItemId(item)}>{catalogItemLabel(item)}</option>)}</select></label>
      <label>Personality<select aria-label="New thread personality" value={personality} disabled={busy}
        onChange={(event) => setPersonality((event.target as HTMLSelectElement).value)}><option value="">默认</option>
        <option value="friendly">Friendly</option><option value="pragmatic">Pragmatic</option></select></label></div>
    <button class="primary-action" type="button" disabled={busy || !source || !catalog || !cwd.startsWith('/')}
      onClick={() => source && catalog && onCreate({ sourceId:source.sourceId,sourceEpoch:catalog.sourceEpoch,cwd,
        model:model || null,personality:personality || null,permissions:permissions || null })}>创建 Thread</button>
  </section></div>;
}

function RawInspector({ events, state, hasMore, onOpen, onMore, onRetry, onCloseError, onCopy }: {
  events: RawEvent[]; state: RequestState; hasMore: boolean; onOpen: () => void; onMore: () => void; onRetry: () => void;
  onCloseError: () => void; onCopy: (event: RawEvent) => void;
}) {
  return <details class="diagnostics" onToggle={(event) => {
    if ((event.currentTarget as HTMLDetailsElement).open && state.phase === 'idle') onOpen();
  }}><summary>Raw Inspector（已脱敏，按需加载）</summary>
    {state.phase === 'loading' && events.length === 0 && <p class="loading" role="status">正在加载 raw events…</p>}
    {state.phase === 'error' && <ErrorNotice message={state.message || 'Raw events 加载失败'} onRetry={onRetry} onClose={onCloseError} />}
    <div class="raw-events">{events.map((event) => <article class="raw-event">
      <div class="raw-event-heading"><strong>event {event.eventSeq}</strong>{badge(event.decodeStatus, 'Decode 状态')}</div>
      <dl class="raw-meta"><dt>source</dt><dd>{event.sourceId}</dd><dt>epoch</dt><dd>{event.sourceEpoch}</dd>
        <dt>sourceSeq</dt><dd>{event.sourceSeq}</dd><dt>method</dt><dd>{event.method}</dd><dt>phase</dt><dd>{event.phase}</dd>
        <dt>durability</dt><dd>{event.durability}</dd><dt>time</dt><dd>{formatTime(event.eventAtMs || event.observedAtMs)}</dd>
        <dt>hash</dt><dd>{event.storedRawHash}</dd><dt>redaction</dt><dd>{jsonText(event.redaction)}</dd></dl>
      <pre>{jsonText(event.raw)}</pre><button type="button" onClick={() => onCopy(event)}>复制已脱敏 JSON</button>
    </article>)}</div>
    {hasMore && <button type="button" class="load-more" onClick={onMore} disabled={state.phase === 'loading'}>加载更多 raw events</button>}
  </details>;
}

function ThreadDetailView({ detail, turns, items, token, health, rawEvents, rawState, rawHasMore, onRawOpen, onRawMore,
  onRawRetry, onRawCloseError, onCopy, onBack, onNavigate, onBlob, focusedItemId, notice, onNoticeClose,
  tuiSessionMode, onOpenSession }: {
  detail: ThreadDetail; turns: Turn[]; items: Item[]; token: string; health?: Health; rawEvents: RawEvent[]; rawState: RequestState;
  rawHasMore: boolean; onRawOpen: () => void; onRawMore: () => void; onRawRetry: () => void; onRawCloseError: () => void;
  onCopy: (event: RawEvent) => void;
  onBack: () => void; onNavigate: (key: string) => void; onBlob: (message: string) => void; focusedItemId?: string;
  notice?: string;
  onNoticeClose: () => void;
  tuiSessionMode: boolean;
  onOpenSession: () => void;
}) {
  const { thread, relations } = detail;
  const grouped = useMemo(() => {
    const map = new Map<string, { turn: Turn | null; items: Item[] }>();
    for (const turn of turns) map.set(turn.turnId, { turn, items: [] });
    for (const item of items) { const key = item.turnId || item.turnScope; if (!map.has(key)) map.set(key, { turn: null, items: [] }); map.get(key)!.items.push(item); }
    return Array.from(map.entries());
  }, [turns, items]);
  const copy = threadDisplay(thread);
  const conversationTail = useRef<HTMLDivElement>(null); const followLatest = useRef(true);
  const previousActivityKey = useRef(''); const [showJumpToLatest, setShowJumpToLatest] = useState(false);
  const [unseenUpdates, setUnseenUpdates] = useState(0);
  const activityKey = `${items.at(-1)?.itemId || ''}:${items.at(-1)?.lastEventSeq || 0}:${items.length}`;
  const pendingCount = detail.pendingRequests.filter((request) => request.state !== 'resolved').length;
  useEffect(() => {
    const scroller = conversationTail.current?.closest('.detail') as HTMLElement | null;
    if (!scroller) return;
    const updateFollowState = () => {
      const following = shouldFollowLatestMessage(scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight);
      followLatest.current = following; setShowJumpToLatest(!following);
      if (following) setUnseenUpdates(0);
    };
    scroller.addEventListener('scroll', updateFollowState, { passive:true });
    followLatest.current = !focusedItemId;
    previousActivityKey.current = activityKey; setShowJumpToLatest(false); setUnseenUpdates(0);
    const frame = requestAnimationFrame(() => {
      if (!focusedItemId) scroller.scrollTo({ top:scroller.scrollHeight, behavior:'auto' });
      updateFollowState();
    });
    return () => { cancelAnimationFrame(frame); scroller.removeEventListener('scroll', updateFollowState); };
  }, [thread.threadKey]);
  useEffect(() => {
    const scroller = conversationTail.current?.closest('.detail') as HTMLElement | null;
    const changed = previousActivityKey.current !== activityKey;
    previousActivityKey.current = activityKey;
    if (!scroller || focusedItemId) return;
    if (!followLatest.current) {
      if (changed) setUnseenUpdates((current) => current + 1);
      setShowJumpToLatest(true); return;
    }
    const frame = requestAnimationFrame(() => scroller.scrollTo({ top:scroller.scrollHeight, behavior:'auto' }));
    return () => cancelAnimationFrame(frame);
  }, [activityKey]);
  function jumpToLatest() {
    const scroller = conversationTail.current?.closest('.detail') as HTMLElement | null;
    followLatest.current = true; setShowJumpToLatest(false); setUnseenUpdates(0);
    scroller?.scrollTo({ top:scroller.scrollHeight, behavior:'smooth' });
  }
  return <div><button class="back-button" type="button" aria-label="返回列表" onClick={onBack}><Icon name="arrow" /> 返回会话</button>
    <div class="thread-header"><div class="thread-title-row"><div><p class="eyebrow">{thread.archived ? '已归档会话' : thread.stale ? '状态可能过期' : '当前会话'}</p>
      <h2>{copy.title}</h2></div></div><p class="thread-meta">{[
        thread.context.runtime.model, thread.context.session.agentNickname, thread.context.session.agentRole,
        thread.project?.name, thread.source, `会话 ${shortThreadId(thread.codexThreadId)}`
      ].filter(Boolean).join(' · ')}</p><CurrentSessionOverview thread={thread}
        turnCount={turns.length} pendingCount={pendingCount} /><div class="thread-relations">
        {relations.parent && <RelationLink label="父会话" relation={relations.parent} onNavigate={onNavigate} />}
        {relations.forkedFrom && <RelationLink label="分支来源" relation={relations.forkedFrom} onNavigate={onNavigate} />}
        {relations.children.map((child) => <RelationLink label="子会话" relation={child} onNavigate={onNavigate} />)}
      </div></div>
    {thread.captureCompleteness !== 'durable_complete' && <div class="capture-banner">{badge(thread.captureCompleteness, 'Thread 捕获完整性')}
      <span>{thread.completenessReasons.length ? thread.completenessReasons.join(' · ') : '该 Thread 的捕获完整性需要关注'}</span></div>}
    <div class="conversation-utilities"><DiagnosticsSummary detail={detail} health={health} /><ContextPanel thread={thread} /></div>
    <div class="timeline">{grouped.length === 0 && <p class="empty-list">此 Thread 尚无可投影 Item。</p>}
      {grouped.map(([key, group], index) => <TurnSection key={key} turn={group.turn} items={group.items} token={token} onBlob={onBlob}
        focusedItemId={focusedItemId} ordinal={index + 1} />)}</div>
    {detail.pendingRequests.some((request) => request.state !== 'resolved') && <div class="request-card-list">
      <div class="notice notice-warning" role="status">此 Thread 有待处理交互；请打开拥有该 Thread 的终端，由官方 TUI 响应。</div>
    </div>}
    <RawInspector events={rawEvents} state={rawState} hasMore={rawHasMore} onOpen={onRawOpen} onMore={onRawMore} onRetry={onRawRetry}
      onCloseError={onRawCloseError} onCopy={onCopy} />
    <div ref={conversationTail} class="conversation-tail" aria-hidden="true" />
    <div class="conversation-dock" aria-label="对话操作区">
      {showJumpToLatest && <ConversationFollowButton unseenUpdates={unseenUpdates} onJump={jumpToLatest} />}
      {notice && <FeedbackNotice message={notice} onClose={onNoticeClose} />}
      {tuiSessionMode ? <div class="session-default-entry" role="status">
        <div><strong>活动输入由真实 Codex TUI 承载</strong><span>Slash、picker、Goal、Plan 与交互请求只在终端会话中响应；History 保持只读。</span></div>
        <button type="button" onClick={onOpenSession}>继续终端会话</button>
      </div> : <div class="session-default-entry" role="status"><div><strong>History 只读</strong>
        <span>启用 V2 Session Runtime 后可从这里继续该 Thread。</span></div></div>}
    </div>
  </div>;
}

function completenessLabel(value: string) {
  return ({ durable_partial: '历史记录不完整', live_partial: '实时记录不完整', metadata_only: '仅元数据', ephemeral_lost: '实时细节已丢失',
    live_complete: '实时完整', durable_complete: '历史完整' } as Record<string, string>)[value] || value.replaceAll('_', ' ');
}

function isSubAgentThread(thread: Thread) {
  return Boolean(thread.agentPath || thread.parentThreadKey || thread.parentThreadId);
}

function threadFlags(thread: Thread) {
  const subAgent = isSubAgentThread(thread);
  return [thread.archived ? '已归档' : thread.stale ? '状态可能过期' : '', subAgent ? '子代理' : '',
    thread.captureCompleteness !== 'durable_complete' ? completenessLabel(thread.captureCompleteness) : ''].filter(Boolean);
}

function ThreadRow({ thread, selected, onSelect }: { thread: Thread; selected?: string; onSelect: (threadKey: string) => void }) {
  const copy = threadDisplay(thread);
  return <button type="button" class={`thread-row${selected === thread.threadKey ? ' selected' : ''}`}
    aria-current={selected === thread.threadKey ? 'true' : undefined} title={copy.excerpt ? `${copy.title} — ${copy.excerpt}` : copy.title}
    onClick={() => onSelect(thread.threadKey)}><div class="thread-row-top"><strong>{copy.title}</strong>
      <span class="thread-row-signals">{thread.status === 'active' && <span class="thread-running" aria-label="正在运行" />}
        {thread.captureCompleteness !== 'durable_complete' && <span class="thread-warning" aria-label={`捕获完整性：${thread.captureCompleteness}`}>需关注</span>}</span></div>
    {copy.excerpt && <p>{copy.excerpt}</p>}<small>{[formatRelativeTime(thread.recencyAtMs), ...threadFlags(thread)].join(' · ')}</small>
  </button>;
}

export function Sidebar({ health, projects, threads, sources, filters, setFilters, searchResults, searchState, selected, onSearch, onSearchResult, onSelect,
  canCreateThread, onNewThread }: {
  health?: Health; projects: ProjectSummary[]; threads: Thread[]; sources: Source[]; filters: Filters; setFilters: (filters: Filters) => void;
  searchResults: SearchResult[] | null; searchState: RequestState; selected?: string; onSearch: (query: string) => void;
  onSearchResult: (result: SearchResult) => void; onSelect: (threadKey: string) => void;
  canCreateThread?: boolean; onNewThread?: () => void;
}) {
  const [search, setSearch] = useState(filters.q);
  const visibleProjects = useMemo(() => projects.filter((project) => threads.some((thread) => thread.project?.key === project.project.key)), [projects, threads]);
  const recentThreads = useMemo(() => threads.filter((thread) => !thread.project && !isSubAgentThread(thread)), [threads]);
  const recentSubAgentThreads = useMemo(() => threads.filter((thread) => !thread.project && isSubAgentThread(thread)), [threads]);
  const reset = () => { setSearch(''); setFilters(defaultFilters); onSearch(''); };
  const compatibility = sources.reduce((summary, source) => ({
    decode: summary.decode + (source.currentEpoch?.decodeErrorCount || 0),
    unknown: summary.unknown + (source.currentEpoch?.unknownEventCount || 0),
    disconnected: summary.disconnected + (['degraded', 'incompatible', 'disconnected', 'offline'].includes(source.status) ? 1 : 0)
  }), { decode: 0, unknown: 0, disconnected: 0 });
  return <aside class="sidebar" aria-label="Thread 导航"><div class="sidebar-head">
    {canCreateThread && <button type="button" class="new-chat-button" onClick={onNewThread}><Icon name="plus" size={17} /><span>新建对话</span></button>}
    <form class="search-row" role="search" onSubmit={(event) => { event.preventDefault(); onSearch(search.trim()); }}>
      <label class="sr-only" for="viewer-search">搜索消息和工具摘要</label><input id="viewer-search" type="search" value={search}
        onInput={(event) => setSearch((event.target as HTMLInputElement).value)} placeholder="搜索会话" /><button type="submit" aria-label="搜索"><Icon name="search" /></button>
    </form></div>
    <details class="filter-panel"><summary><span>筛选</span><span>{[filters.source, filters.status, filters.completeness, filters.archived].filter(Boolean).length || '全部'}</span></summary>
    <div class="filters" aria-label="Thread 筛选">
      <label><span>数据源</span><select value={filters.source} onChange={(event) => setFilters({ ...filters, source: (event.target as HTMLSelectElement).value })}>
        <option value="">全部 source</option>{sources.map((source) => <option value={source.sourceId}>{source.sourceId}</option>)}</select></label>
      <label><span>运行状态</span><select value={filters.status} onChange={(event) => setFilters({ ...filters, status: (event.target as HTMLSelectElement).value })}>
        <option value="">全部状态</option><option>active</option><option>idle</option><option>not_loaded</option></select></label>
      <label><span>捕获完整性</span><select value={filters.completeness} onChange={(event) => setFilters({ ...filters, completeness: (event.target as HTMLSelectElement).value })}>
        <option value="">全部完整性</option><option>durable_complete</option><option>durable_partial</option><option>live_complete</option>
        <option>live_partial</option><option>metadata_only</option><option>ephemeral_lost</option></select></label>
      <label><span>归档状态</span><select value={filters.archived} onChange={(event) => setFilters({ ...filters, archived: (event.target as HTMLSelectElement).value })}>
        <option value="">当前与归档</option><option value="false">仅当前</option><option value="true">仅归档</option></select></label>
    </div></details>
    {(filters.q || filters.source || filters.status || filters.completeness || filters.archived) && <div class="active-filters"><span>当前筛选：{[
      filters.q && `搜索“${filters.q}”`, filters.source, filters.status, filters.completeness,
      filters.archived && (filters.archived === 'true' ? '归档' : '当前')].filter(Boolean).join(' · ')}</span><button type="button" onClick={reset}>全部重置</button></div>}
    <div class="source-summary" title={`后端 ${health?.status || 'loading'} · ${sources.length} 个数据源`}><span>项目</span><span>{visibleProjects.length} 个项目 · {threads.length} 个会话</span></div>
    {(compatibility.decode > 0 || compatibility.unknown > 0 || compatibility.disconnected > 0) && <div class="compatibility-notice" role="status">
      数据兼容性提示：{[compatibility.decode && `${compatibility.decode} 条解析失败`, compatibility.unknown && `${compatibility.unknown} 条未知事件`,
        compatibility.disconnected && `${compatibility.disconnected} 个数据源未连接`].filter(Boolean).join(' · ')}。详情请查看 Thread 诊断。
    </div>}
    <div aria-live="polite" class="search-status">{searchState.phase === 'loading' && '正在搜索…'}
      {searchState.phase === 'error' && `搜索失败：${searchState.message}`}
      {searchResults && searchState.phase === 'success' && `查询“${filters.q}”找到 ${searchResults.length} 条结果`}</div>
    {searchResults && <div class="search-results" aria-label="搜索结果">{searchResults.length === 0 && <p class="empty-list">没有匹配结果</p>}
      {searchResults.map((result) => <button type="button" class="search-result" onClick={() => onSearchResult(result)}>
        <strong>{result.turnId ? `Turn ${result.turnId}` : 'Thread 命中'}</strong><span>{result.snippet || '无摘要'}</span></button>)}</div>}
    {!searchResults && <div class="project-tree">{visibleProjects.length === 0 && recentThreads.length === 0 && recentSubAgentThreads.length === 0 && <p class="empty-list">尚未导入或没有符合筛选条件的 Thread。</p>}
      {visibleProjects.map((project) => { const projectThreads = threads.filter((thread) => thread.project?.key === project.project.key);
        const primaryThreads = projectThreads.filter((thread) => !isSubAgentThread(thread));
        const subAgentThreads = projectThreads.filter(isSubAgentThread);
        return <details class="project-group" open>
        <summary class="project-heading" title={project.project.path}><Icon name="chevron" size={13} /><Icon name="folder" size={15} />
          <span class="project-title"><strong>{project.project.name}</strong><small>{project.project.path}</small></span>
          <span class="project-count">{projectThreads.length}</span></summary><div class="thread-list primary-thread-list">
          {primaryThreads.map((thread) => <ThreadRow thread={thread} selected={selected} onSelect={onSelect} />)}
          {primaryThreads.length === 0 && <p class="empty-project">没有主会话</p>}
          {subAgentThreads.length > 0 && <details class="subagent-group" open={subAgentThreads.some((thread) => thread.threadKey === selected)}>
            <summary><Icon name="chevron" size={12} /><span>子代理记录</span><span>{subAgentThreads.length}</span></summary>
            <div class="subagent-list">{subAgentThreads.map((thread) => <ThreadRow thread={thread} selected={selected} onSelect={onSelect} />)}</div>
          </details>}
        </div>
      </details>; })}
      {(recentThreads.length > 0 || recentSubAgentThreads.length > 0) && <section class="recent-section" aria-labelledby="recent-heading">
        <h2 id="recent-heading">最近</h2><div class="thread-list recent-thread-list">
          {recentThreads.map((thread) => <ThreadRow thread={thread} selected={selected} onSelect={onSelect} />)}
          {recentSubAgentThreads.length > 0 && <details class="subagent-group" open={recentSubAgentThreads.some((thread) => thread.threadKey === selected)}>
            <summary><Icon name="chevron" size={12} /><span>子代理记录</span><span>{recentSubAgentThreads.length}</span></summary>
            <div class="subagent-list">{recentSubAgentThreads.map((thread) => <ThreadRow thread={thread} selected={selected} onSelect={onSelect} />)}</div>
          </details>}
        </div>
      </section>}
    </div>}
  </aside>;
}

async function downloadBlob(blobId: string, token: string, onBlob: (message: string) => void) {
  try {
    const response = await fetch(`/v1/blobs/${encodeURIComponent(blobId)}`, { headers: token ? { Authorization: `Bearer ${token}` } : {}, credentials: 'same-origin', cache: 'no-store' });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const url = URL.createObjectURL(await response.blob()); const link = document.createElement('a');
    link.href = url; link.download = 'observer-redacted-blob.json'; link.click(); URL.revokeObjectURL(url);
  } catch (error) { onBlob(`Blob 下载失败：${requestMessage(error)}`); }
}

export function App() {
  const initialPairCode = new URLSearchParams(window.location.hash.slice(1)).get('pair');
  const [token, setToken] = useState(savedToken);
  const [api, setApi] = useState<Api | null>(() => deferInitialDashboardForPairing(window.location.hash) ? null : connect(savedToken));
  const [authChecking, setAuthChecking] = useState(true);
  const [authError, setAuthError] = useState('');
  const [health, setHealth] = useState<Health>();
  const [threads, setThreads] = useState<Thread[]>([]);
  const [projects, setProjects] = useState<ProjectSummary[]>([]);
  const [sources, setSources] = useState<Source[]>([]);
  const [dashboardState, setDashboardState] = useState<RequestState>({ phase: 'idle' });
  const [transport, setTransport] = useState<TransportState>('connecting');
  const [selected, setSelected] = useState<string>();
  const [detail, setDetail] = useState<ThreadDetail>();
  const [turns, setTurns] = useState<Turn[]>([]);
  const [items, setItems] = useState<Item[]>([]);
  const [detailState, setDetailState] = useState<RequestState>({ phase: 'idle' });
  const [rawEvents, setRawEvents] = useState<RawEvent[]>([]);
  const [rawState, setRawState] = useState<RequestState>({ phase: 'idle' });
  const [rawHasMore, setRawHasMore] = useState(false);
  const [filters, setFilters] = useState<Filters>(defaultFilters);
  const [searchResults, setSearchResults] = useState<SearchResult[] | null>(null);
  const [searchState, setSearchState] = useState<RequestState>({ phase: 'idle' });
  const [notice, setNotice] = useState('');
  const [sessionPreviewOpen, setSessionPreviewOpen] = useState(false);
  const [sessionIntent, setSessionIntent] = useState<SessionIntent>({ mode:'new' });
  const [focusTarget, setFocusTarget] = useState<{ turnId?: string; itemId?: string }>();
  const refreshTimer = useRef<number>();
  const detailController = useRef<AbortController>();
  const searchController = useRef<AbortController>();
  const rawController = useRef<AbortController>();
  const selectedRef = useRef<string>();

  function unauthorize(message: string) {
    sessionStorage.removeItem('observer-token'); setToken(''); setApi(null); setAuthError(message); setAuthChecking(false);
  }

  useEffect(() => {
    if (!initialPairCode) return;
    history.replaceState(null, '', `${location.pathname}${location.search}`); setAuthChecking(true);
    fetch('/v1/auth/pair', { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ code: initialPairCode }) })
      .then(async (response) => { const body = await response.json(); if (!response.ok) throw new Error(body?.error?.message || `HTTP ${response.status}`);
        sessionStorage.removeItem('observer-token'); setToken(''); setApi(connect('')); setTransport('connecting'); })
      .catch((error) => unauthorize(requestMessage(error)));
  }, []);

  useEffect(() => {
    if (!api) return;
    let cancelled = false;
    async function refresh() {
      if (!api || cancelled) return;
      setDashboardState((current) => current.phase === 'success' ? current : { phase: 'loading' });
      try {
        const dashboard = await loadDashboard(api);
        if (cancelled) return;
        setHealth({ ...dashboard.health.data, asOfEventSeq: dashboard.health.asOfEventSeq });
        setThreads(dashboard.threads.data); setProjects(dashboard.projects.data); setSources(dashboard.sources.data);
        setDashboardState({ phase: 'success' }); setAuthChecking(false); setAuthError('');
      } catch (error) {
        if (cancelled || isAbort(error)) return;
        if (error instanceof ApiError && error.status === 401) return unauthorize(error.message);
        setDashboardState({ phase: 'error', message: requestMessage(error) }); setAuthChecking(false);
      }
    }
    void refresh();
    const streamController = new AbortController();
    void reconnectingStream(api, '/v2/stream', () => {
      window.clearTimeout(refreshTimer.current); refreshTimer.current = window.setTimeout(() => {
        void refresh();
        const threadKey = selectedRef.current;
        if (threadKey) void selectThread(threadKey, undefined, true);
      }, 150);
    }, setTransport, streamController.signal).catch((error) => {
      if (error instanceof ApiError && error.status === 401) unauthorize(error.message);
      else if (!isAbort(error)) setTransport('disconnected');
    });
    const interval = window.setInterval(refresh, 15_000);
    return () => { cancelled = true; streamController.abort(); window.clearInterval(interval); window.clearTimeout(refreshTimer.current); };
  }, [api, token]);

  useEffect(() => () => {
    detailController.current?.abort(); searchController.current?.abort(); rawController.current?.abort();
  }, []);
  useEffect(() => {
    const target = focusTarget;
    if (detailState.phase !== 'success' || !target) return;
    const id = target.itemId ? `item-${encodeURIComponent(target.itemId)}` : target.turnId ? `turn-${encodeURIComponent(target.turnId)}` : '';
    if (id) requestAnimationFrame(() => document.getElementById(id)?.scrollIntoView({ block: 'center' }));
  }, [detailState.phase, focusTarget]);

  async function selectThread(threadKey: string, target?: { turnId?: string; itemId?: string }, preserve = false) {
    if (!api) return;
    detailController.current?.abort(); const controller = new AbortController(); detailController.current = controller;
    selectedRef.current = threadKey;
    if (!preserve) {
      rawController.current?.abort(); setSelected(threadKey); setFocusTarget(target); setDetail(undefined); setTurns([]); setItems([]); setRawEvents([]);
      setRawState({ phase: 'idle' }); setRawHasMore(false); setDetailState({ phase: 'loading' });
    }
    try {
      const loaded = await loadThread(api, threadKey, controller.signal);
      if (controller.signal.aborted) return;
      setDetail(loaded.detail.data); setTurns(loaded.turns.data); setItems(loaded.items.data); setDetailState({ phase: 'success' });
    } catch (error) {
      if (isAbort(error)) return;
      if (error instanceof ApiError && error.status === 401) return unauthorize(error.message);
      if (preserve) setNotice(`实时刷新失败：${requestMessage(error)}`);
      else setDetailState({ phase: 'error', message: requestMessage(error) });
    }
  }

  async function loadRaw(reset = false) {
    if (!api || !selected || rawState.phase === 'loading') return;
    rawController.current?.abort(); const controller = new AbortController(); rawController.current = controller;
    setRawState({ phase: 'loading' }); const after = reset ? 0 : rawEvents.at(-1)?.eventSeq || 0;
    try {
      const page = await loadEventPage(api, selected, after, controller.signal);
      if (controller.signal.aborted) return;
      setRawEvents((current) => reset ? page.data : [...current, ...page.data]); setRawHasMore(page.data.length === 100); setRawState({ phase: 'success' });
    } catch (error) {
      if (isAbort(error)) return;
      if (error instanceof ApiError && error.status === 401) return unauthorize(error.message);
      setRawState({ phase: 'error', message: requestMessage(error) });
    }
  }

  function handleConnect(value: string) {
    setAuthError(''); setAuthChecking(true); if (value) sessionStorage.setItem('observer-token', value);
    setToken(value); setApi(connect(value)); setTransport('connecting');
  }

  async function handleSearch(query: string) {
    if (!api) return;
    searchController.current?.abort();
    if (!query) { setSearchResults(null); setSearchState({ phase: 'idle' }); setFilters((current) => ({ ...current, q: '' })); return; }
    const controller = new AbortController(); searchController.current = controller; setSearchState({ phase: 'loading' });
    setFilters((current) => ({ ...current, q: query }));
    try {
      const results = await api.getAll<SearchResult>(`/v1/search?q=${encodeURIComponent(query)}&limit=200`, controller.signal);
      if (controller.signal.aborted) return; setSearchResults(results.data); setSearchState({ phase: 'success' });
    } catch (error) {
      if (isAbort(error)) return;
      if (error instanceof ApiError && error.status === 401) return unauthorize(error.message);
      setSearchState({ phase: 'error', message: requestMessage(error) });
    }
  }

  const filteredThreads = useMemo(() => threads.filter((thread) => (!filters.source || thread.storeSourceId === filters.source)
    && (!filters.status || thread.status === filters.status) && (!filters.completeness || thread.captureCompleteness === filters.completeness)
    && (!filters.archived || String(thread.archived) === filters.archived)), [threads, filters]);
  const visibleThreadKeys = useMemo(() => new Set(filteredThreads.map((thread) => thread.threadKey)), [filteredThreads]);
  const visibleSearchResults = useMemo(() => searchResults?.filter((result) => visibleThreadKeys.has(result.threadKey)) ?? null,
    [searchResults, visibleThreadKeys]);
  const sessionLabel = token ? 'Bearer' : tailscaleViewer ? 'Tailscale' : 'Cookie';
  const transportText = transport === 'live' ? `${sessionLabel} · 实时已连接`
    : transport === 'disconnected' ? `${sessionLabel} · 实时已断开，正在重试` : `${sessionLabel} · 正在连接实时更新`;
  const tuiSessionMode = health?.sessionKernel?.configuredMode === 'tui'
    && health.sessionKernel.workerAvailable && health.sessionKernel.cliAvailable;

  if (!api) return <main><header class="topbar"><div class="brand"><span class="brand-mark"><Icon name="codex" size={18} /></span><h1>Codex Observer</h1><span class="readonly-label">待认证</span></div></header>
    <AuthPanel onConnect={handleConnect} error={authError} checking={authChecking} /></main>;

  return <main><header class="topbar"><div class="brand"><span class="brand-mark"><Icon name="codex" size={18} /></span><h1>Codex Observer</h1>
    <span class="readonly-label">{health?.control?.enabled ? 'V2 控制' : '只读'}</span></div>
    <div class="status-stack">{health?.sessionKernel?.workerAvailable && health.sessionKernel.cliAvailable &&
      <button type="button" class="session-preview-button" onClick={() => { setSessionIntent({mode:'new'}); setSessionPreviewOpen(true); }}>终端会话</button>}
      <div class={`health health-${health?.status || 'loading'}`} title={`后端 ${health?.status || 'loading'} · event ${health?.asOfEventSeq || 0}`}>
      <span class="status-dot" />本机数据</div><div class={`transport transport-${transport}`}>{transportText}</div></div></header>
    {dashboardState.phase === 'loading' && !health && <div class="page-status" role="status">正在加载 Dashboard…</div>}
    {dashboardState.phase === 'error' && <ErrorNotice message={`Dashboard：${dashboardState.message}`} onRetry={() => setApi(connect(token))}
      onClose={() => setDashboardState({ phase: 'idle' })} />}
    {tailscaleViewer && health?.control?.tailscaleMutationAccess && <div class="control-risk" role="alert">Tailscale 风险：当前经验证身份拥有与本机登录相同的 V2 mutation 权限。</div>}
    {notice && !detail && <div class="page-notice-dock"><FeedbackNotice message={notice} onClose={() => setNotice('')} /></div>}
    {sessionPreviewOpen && <SessionShell api={api} intent={sessionIntent} projects={projects}
      onClose={() => setSessionPreviewOpen(false)} />}
    <div class={`workspace${selected ? ' detail-active' : ''}`}><Sidebar health={health} projects={projects} threads={filteredThreads} sources={sources}
      filters={filters} setFilters={(value) => { setFilters(value); if (!value.q) setSearchResults(null); }} searchResults={visibleSearchResults}
      searchState={searchState} selected={selected} onSearch={handleSearch}
      onSearchResult={(result) => selectThread(result.threadKey, { turnId: result.turnId, itemId: result.itemId })} onSelect={(key) => selectThread(key)}
      canCreateThread={health?.control?.enabled} onNewThread={() => {
        if (tuiSessionMode) { setSessionIntent({mode:'new',fresh:true}); setSessionPreviewOpen(true); }
        else setNotice('Session Runtime 未启用；History 保持只读');
      }} />
      <section class="detail" aria-busy={detailState.phase === 'loading'}><div class="detail-inner">
        {detailState.phase === 'loading' && <div class="detail-loading" role="status"><p class="eyebrow">THREAD → TURN → ITEM</p><h2>正在加载所选 Thread…</h2></div>}
        {detailState.phase === 'error' && <ErrorNotice message={`Thread：${detailState.message}`} onRetry={() => selected && selectThread(selected, focusTarget)}
          onClose={() => setDetailState({ phase: 'idle' })} />}
        {!selected && <div class="empty-state"><span class="empty-mark"><Icon name="codex" size={28} /></span><h2>选择一个会话</h2>
          <p>从左侧项目中打开 Codex 会话，查看对话与执行过程。</p></div>}
        {detail && detailState.phase === 'success' && <ThreadDetailView detail={detail} turns={turns} items={items} token={token} health={health}
          rawEvents={rawEvents} rawState={rawState} rawHasMore={rawHasMore} onRawOpen={() => void loadRaw(true)} onRawMore={() => void loadRaw(false)}
          onRawRetry={() => void loadRaw(rawEvents.length === 0)} onRawCloseError={() => setRawState({ phase: rawEvents.length ? 'success' : 'idle' })}
          onCopy={(event) => navigator.clipboard.writeText(jsonText(event.raw))
            .then(() => setNotice('已复制脱敏 JSON')).catch(() => setNotice('复制失败'))}
          onBack={() => { detailController.current?.abort(); rawController.current?.abort(); selectedRef.current = undefined; setSelected(undefined); setDetail(undefined); setDetailState({ phase: 'idle' }); }}
          onNavigate={(key) => selectThread(key)} onBlob={setNotice} focusedItemId={focusTarget?.itemId}
          notice={notice} onNoticeClose={() => setNotice('')}
          tuiSessionMode={Boolean(tuiSessionMode)} onOpenSession={() => {
            const cwd = detail.thread.context.runtime.cwd || detail.thread.cwdDisplay || '';
            setSessionIntent({mode:'resume',storeSourceId:detail.thread.storeSourceId,codexThreadId:detail.thread.codexThreadId,cwd});
            setSessionPreviewOpen(true);
          }} />}
      </div></section>
    </div>
  </main>;
}
