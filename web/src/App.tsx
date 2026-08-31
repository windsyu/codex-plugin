import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { Api, ApiError, connect, loadDashboard, loadEventPage, loadThread, reconnectingStream } from './api';
import { renderMarkdown } from './lib/markdown';
import { coalesceActivities, itemPayload, presentItem, summarizeActivities } from './presentation';
import type { ActivityEntry } from './presentation';
import type { ControlCatalog, ControllerSource, GatewayCommand, Health, Item, PendingRequest, ProjectSummary, RawEvent, SearchResult, Source, Thread, ThreadDetail, Turn } from './types';

type RequestPhase = 'idle' | 'loading' | 'success' | 'error';
type TransportState = 'connecting' | 'live' | 'disconnected';
interface Filters { source: string; status: string; completeness: string; archived: string; q: string; }
interface RequestState { phase: RequestPhase; message?: string; }

const defaultFilters: Filters = { source: '', status: '', completeness: '', archived: '', q: '' };
const savedToken = sessionStorage.getItem('observer-token') || '';
const tailscaleViewer = window.location.protocol === 'https:' && window.location.hostname.endsWith('.ts.net');

function Icon({ name, size = 16 }: { name: 'codex' | 'folder' | 'search' | 'chevron' | 'arrow'; size?: number }) {
  const paths = {
    codex: <><path d="M8 1.5 13.6 4.7v6.6L8 14.5l-5.6-3.2V4.7L8 1.5Z"/><path d="m5.2 6.1 2.8-1.6 2.8 1.6v3.8L8 11.5 5.2 9.9V6.1Z"/></>,
    folder: <path d="M1.8 4.1h4.6l1.3 1.5h6.5v7.2H1.8V4.1Z"/>,
    search: <><circle cx="7" cy="7" r="4.3"/><path d="m10.2 10.2 3.3 3.3"/></>,
    chevron: <path d="m6 3.5 4.5 4.5L6 12.5"/>,
    arrow: <><path d="M13.5 8h-11M6.5 4 2.5 8l4 4"/></>
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

export function commandNotice(command: GatewayCommand) {
  return command.state === 'outcome_unknown'
    ? '操作结果未知：请刷新状态，系统不会自动重放'
    : `控制操作：${command.state}`;
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
  const summary = itemSummary(item);
  const phase = String(payloadOf(item).phase || '');
  const assistant = presentation.role !== 'user';
  return <article class={`dialogue-message dialogue-${presentation.role || 'assistant'}`} id={`item-${encodeURIComponent(item.itemId)}`}
    data-item-id={item.itemId}>
    {assistant && <span class="dialogue-avatar"><Icon name="codex" size={15} /></span>}
    <div class="dialogue-body"><header><strong>{presentation.label}</strong><span>{formatClock(item.startedAtMs || item.completedAtMs)}{phase ? ` · ${phaseLabel(phase)}` : ''}</span></header>
      {summary ? <div class="dialogue-content markdown" dangerouslySetInnerHTML={{ __html: renderMarkdown(summary) }} />
        : <p class="empty-message">消息正文未保留</p>}
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
    for (const candidate of [item.itemId, raw.clientUserMessageId, payload.clientUserMessageId, provenance.clientUserMessageId]) {
      if (typeof candidate === 'string' && candidate) ids.add(candidate);
    }
  }
  return ids;
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
  const dialogue = items.filter((item) => presentItem(item).group === 'dialogue');
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

function requestPayloadSummary(request: PendingRequest) {
  const payload = record(request.payload);
  const fields = ['command', 'cwd', 'grantRoot', 'reason', 'permissions', 'questions', 'serverName', 'message', 'requestedSchema'];
  return Object.fromEntries(fields.filter((field) => payload[field] != null).map((field) => [field, payload[field]]));
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

export function Composer({ catalog, disabledReason, busy, onSend, onInterrupt }: {
  catalog?: ControlCatalog; disabledReason?: string; busy: boolean; onSend: (text: string, images: File[]) => void; onInterrupt: () => void;
}) {
  const [text, setText] = useState(''); const [images, setImages] = useState<File[]>([]); const disabled = Boolean(disabledReason) || busy;
  return <section class="composer" aria-label="Codex 控制 Composer"><div class="composer-status">
    <span>{disabledReason || (catalog?.activeTurnId ? `活动 Turn ${shortThreadId(catalog.activeTurnId)}` : 'Source 已连接')}</span>
    {catalog?.activeTurnId && <button type="button" disabled={disabled} onClick={onInterrupt}>Interrupt</button>}</div>
    <textarea value={text} disabled={disabled} placeholder="发送消息，输入 / 查看可用命令"
      onInput={(event) => setText((event.target as HTMLTextAreaElement).value)} onKeyDown={(event) => {
        if (event.key === 'Enter' && !event.shiftKey && (text.trim() || images.length)) { event.preventDefault(); onSend(text, images); setText(''); setImages([]); }
      }} />
    {text.startsWith('/') && <div class="slash-palette">{catalog?.slashCommands.filter((command) => command.name.startsWith(text.split(/\s/)[0])).map((command) =>
      <button type="button" onClick={() => setText(`${command.name} `)}>{command.name}<small>{command.capability}</small></button>)}</div>}
    {images.length > 0 && <div class="image-preview">{images.map((file) => <span>{file.name} · {(file.size / 1024 / 1024).toFixed(1)} MiB</span>)}</div>}
    <div class="composer-footer"><label class="image-picker">添加图片<input type="file" accept="image/png,image/jpeg,image/webp,image/gif" multiple disabled={disabled}
      onChange={(event) => setImages(Array.from((event.target as HTMLInputElement).files || []).slice(0, 4))} /></label>
      <span>Enter 发送 · Shift+Enter 换行</span><button type="button" disabled={disabled || (!text.trim() && !images.length)}
      onClick={() => { onSend(text, images); setText(''); setImages([]); }}>发送</button></div>
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
  const planMode = catalog.collaborationMode?.mode === 'plan'; const goalStatus = catalog.goal?.status;
  return <section class="control-settings" aria-label="Codex 控制设置">
    <div class="setting-row">
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
    <div class="mode-row"><span>Plan：{planMode ? '已启用' : '未启用'}</span><button type="button" disabled={busy || planMode}
      onClick={() => onSlash('/plan')}>进入 Plan</button><span>Goal：{goalStatus || '未设置'}</span>
      <input aria-label="Goal objective" value={goal} placeholder="设置 Goal" disabled={busy}
        onInput={(event) => setGoal((event.target as HTMLInputElement).value)} />
      <button type="button" disabled={busy || !goal.trim()} onClick={() => { onSlash(`/goal ${goal}`); setGoal(''); }}>设置</button>
      {goalStatus === 'active' && <button type="button" disabled={busy} onClick={() => onSlash('/goal pause')}>暂停</button>}
      {goalStatus === 'paused' && <button type="button" disabled={busy} onClick={() => onSlash('/goal resume')}>恢复</button>}
      {goalStatus && <button type="button" disabled={busy} onClick={() => onSlash('/goal clear')}>清除</button>}
    </div>
  </section>;
}

export function NewThreadDialog({ sources, catalog, busy, error, onSource, onClose, onCreate }: {
  sources: ControllerSource[]; catalog?: ControlCatalog; busy: boolean; error?: string;
  onSource: (sourceId: string) => void; onClose: () => void;
  onCreate: (input: { sourceId: string; sourceEpoch: string; cwd: string; model: string | null; personality: string | null; permissions: string | null }) => void;
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
  onRawRetry, onRawCloseError, onCopy, onBack, onNavigate, onBlob, focusedItemId, catalog, controlBusy, controlError,
  latestCommand, optimisticMessages, onSend, onInterrupt, onSetting, onRequestAction }: {
  detail: ThreadDetail; turns: Turn[]; items: Item[]; token: string; health?: Health; rawEvents: RawEvent[]; rawState: RequestState;
  rawHasMore: boolean; onRawOpen: () => void; onRawMore: () => void; onRawRetry: () => void; onRawCloseError: () => void;
  onCopy: (event: RawEvent) => void;
  onBack: () => void; onNavigate: (key: string) => void; onBlob: (message: string) => void; focusedItemId?: string;
  catalog?: ControlCatalog; controlBusy: boolean; controlError?: string; onSend: (text: string, images: File[]) => void; onInterrupt: () => void;
  latestCommand?: GatewayCommand;
  optimisticMessages: OptimisticMessage[];
  onSetting: (capability: string, value: string) => void;
  onRequestAction: (request: PendingRequest, action: Record<string, unknown>) => void;
}) {
  const { thread, relations } = detail;
  const grouped = useMemo(() => {
    const map = new Map<string, { turn: Turn | null; items: Item[] }>();
    for (const turn of turns) map.set(turn.turnId, { turn, items: [] });
    for (const item of items) { const key = item.turnId || item.turnScope; if (!map.has(key)) map.set(key, { turn: null, items: [] }); map.get(key)!.items.push(item); }
    return Array.from(map.entries());
  }, [turns, items]);
  const copy = threadDisplay(thread);
  return <div><button class="back-button" type="button" aria-label="返回列表" onClick={onBack}><Icon name="arrow" /> 返回会话</button>
    <div class="thread-header"><p class="eyebrow">{thread.archived ? '已归档会话' : thread.stale ? '状态可能过期' : '当前会话'}</p>
      <h2>{copy.title}</h2><p class="thread-meta">{[
        thread.context.runtime.model, thread.context.session.agentNickname, thread.context.session.agentRole,
        thread.project?.name, thread.source, `会话 ${shortThreadId(thread.codexThreadId)}`
      ].filter(Boolean).join(' · ')}</p><div class="thread-relations">
        {relations.parent && <RelationLink label="父会话" relation={relations.parent} onNavigate={onNavigate} />}
        {relations.forkedFrom && <RelationLink label="分支来源" relation={relations.forkedFrom} onNavigate={onNavigate} />}
        {relations.children.map((child) => <RelationLink label="子会话" relation={child} onNavigate={onNavigate} />)}
      </div></div>
    {thread.captureCompleteness !== 'durable_complete' && <div class="capture-banner">{badge(thread.captureCompleteness, 'Thread 捕获完整性')}
      <span>{thread.completenessReasons.length ? thread.completenessReasons.join(' · ') : '该 Thread 的捕获完整性需要关注'}</span></div>}
    <div class="conversation-utilities"><DiagnosticsSummary detail={detail} health={health} /><ContextPanel thread={thread} /></div>
    <div class="timeline">{grouped.length === 0 && optimisticMessages.length === 0 && <p class="empty-list">此 Thread 尚无可投影 Item。</p>}
      {grouped.map(([key, group], index) => <TurnSection key={key} turn={group.turn} items={group.items} token={token} onBlob={onBlob}
        focusedItemId={focusedItemId} ordinal={index + 1} />)}
      {optimisticMessages.map((message) => <OptimisticMessageCard key={message.clientUserMessageId} message={message} />)}</div>
    <div class="request-card-list">{detail.pendingRequests.filter((request) => request.state !== 'resolved').map((request) =>
      <PendingRequestCard request={request} busy={controlBusy} onAction={onRequestAction} />)}</div>
    {latestCommand && <div class={`command-status command-${latestCommand.state}`} role="status"><strong>最近命令</strong>
      <span>{latestCommand.commandId.slice(0, 8)} · {latestCommand.state}</span>{latestCommand.error && <span>{latestCommand.error.code} · {latestCommand.error.message}</span>}</div>}
    {catalog?.threadLoaded && <ControlSettings thread={thread} catalog={catalog} busy={controlBusy} onSetting={onSetting}
      onSlash={(command) => onSend(command, [])} />}
    <Composer catalog={catalog} busy={controlBusy} disabledReason={controlError || (!catalog?.threadLoaded ? '当前 Thread 未加载到可控 source' : undefined)}
      onSend={onSend} onInterrupt={onInterrupt} />
    <RawInspector events={rawEvents} state={rawState} hasMore={rawHasMore} onOpen={onRawOpen} onMore={onRawMore} onRetry={onRawRetry}
      onCloseError={onRawCloseError} onCopy={onCopy} />
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
      {thread.captureCompleteness !== 'durable_complete' && <span class="thread-warning" aria-label={`捕获完整性：${thread.captureCompleteness}`}>需关注</span>}</div>
    {copy.excerpt && <p>{copy.excerpt}</p>}<small>{[formatRelativeTime(thread.recencyAtMs), ...threadFlags(thread)].join(' · ')}</small>
  </button>;
}

export function Sidebar({ health, projects, threads, sources, filters, setFilters, searchResults, searchState, selected, onSearch, onSearchResult, onSelect }: {
  health?: Health; projects: ProjectSummary[]; threads: Thread[]; sources: Source[]; filters: Filters; setFilters: (filters: Filters) => void;
  searchResults: SearchResult[] | null; searchState: RequestState; selected?: string; onSearch: (query: string) => void;
  onSearchResult: (result: SearchResult) => void; onSelect: (threadKey: string) => void;
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
  const [token, setToken] = useState(savedToken);
  const [api, setApi] = useState<Api | null>(() => connect(savedToken));
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
  const [controlCatalog, setControlCatalog] = useState<ControlCatalog>();
  const [controlError, setControlError] = useState('Controller 状态尚未加载');
  const [controlBusy, setControlBusy] = useState(false);
  const [latestCommand, setLatestCommand] = useState<GatewayCommand>();
  const [optimisticMessages, setOptimisticMessages] = useState<OptimisticMessage[]>([]);
  const [newThreadOpen, setNewThreadOpen] = useState(false);
  const [newThreadSources, setNewThreadSources] = useState<ControllerSource[]>([]);
  const [newThreadCatalog, setNewThreadCatalog] = useState<ControlCatalog>();
  const [newThreadError, setNewThreadError] = useState('');
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
    const code = new URLSearchParams(window.location.hash.slice(1)).get('pair');
    if (!code) return;
    history.replaceState(null, '', `${location.pathname}${location.search}`); setAuthChecking(true);
    fetch('/v1/auth/pair', { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ code }) })
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

  useEffect(() => () => { detailController.current?.abort(); searchController.current?.abort(); rawController.current?.abort(); }, []);
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
      setControlCatalog(undefined); setLatestCommand(undefined); setControlError('正在匹配可控 source');
      setRawState({ phase: 'idle' }); setRawHasMore(false); setDetailState({ phase: 'loading' });
    }
    try {
      const loaded = await loadThread(api, threadKey, controller.signal);
      if (controller.signal.aborted) return;
      setDetail(loaded.detail.data); setTurns(loaded.turns.data); setItems(loaded.items.data); setDetailState({ phase: 'success' });
      setOptimisticMessages((current) => reconcileOptimisticMessages(current, threadKey, loaded.items.data));
      try {
        const snapshots = await api.get<ControllerSource[]>('/v2/control/sources', controller.signal);
        const catalogs = await Promise.allSettled(snapshots.data.filter((source) => source.state === 'ready').map((source) =>
          api.get<ControlCatalog>(`/v2/control/catalog?sourceId=${encodeURIComponent(source.sourceId)}&threadKey=${encodeURIComponent(threadKey)}`, controller.signal)));
        const matched = catalogs.flatMap((result) => result.status === 'fulfilled' && result.value.data.threadLoaded ? [result.value.data] : [])[0];
        if (matched) {
          setControlCatalog(matched); setControlError('');
          try {
            const commands = await api.get<GatewayCommand[]>(`/v2/commands?threadKey=${encodeURIComponent(threadKey)}&limit=1`, controller.signal);
            setLatestCommand(commands.data[0]);
          } catch (error) { if (isAbort(error)) return; }
        }
        else setControlError(snapshots.data.length ? '当前 Thread 未加载到可控 source' : 'Controller 已关闭或 source 离线');
      } catch (error) { if (!isAbort(error)) setControlError(requestMessage(error)); }
    } catch (error) {
      if (isAbort(error)) return;
      if (error instanceof ApiError && error.status === 401) return unauthorize(error.message);
      if (preserve) setNotice(`实时刷新失败：${requestMessage(error)}`);
      else setDetailState({ phase: 'error', message: requestMessage(error) });
    }
  }

  async function runControl(path: string, body: unknown) {
    if (!api) return;
    setControlBusy(true); setNotice('');
    try {
      const response = await api.post<GatewayCommand>(path, body, crypto.randomUUID());
      setLatestCommand(response.data);
      setNotice(commandNotice(response.data));
      if (selected) await selectThread(selected, focusTarget);
      return response.data;
    } catch (error) { setNotice(`控制失败：${requestMessage(error)}`); return undefined; }
    finally { setControlBusy(false); }
  }

  async function sendInput(text: string, images: File[] = []) {
    if (!detail || !controlCatalog) return;
    const threadKey = detail.thread.threadKey;
    const clientUserMessageId = crypto.randomUUID();
    const optimistic = !text.startsWith('/') || text.startsWith('//');
    if (optimistic) {
      setOptimisticMessages((current) => [...current, {
        threadKey, clientUserMessageId, text, imageCount:images.length, createdAtMs:Date.now(), state:'sending'
      }].slice(-50));
    }
    setControlBusy(true);
    try {
      const uploads = await Promise.all(images.map((image) => api!.uploadImage(image, crypto.randomUUID())));
      const command = await runControl(`/v2/threads/${encodeURIComponent(threadKey)}/inputs`, {
      sourceId: controlCatalog.sourceId, sourceEpoch: controlCatalog.sourceEpoch,
      codexThreadId: detail.thread.codexThreadId, expectedTurnId: controlCatalog.activeTurnId,
      clientUserMessageId, text, uploadIds: uploads.map((upload) => upload.data.uploadId)
      });
      if (optimistic) setOptimisticMessages((current) => current.map((message) => message.clientUserMessageId === clientUserMessageId
        ? { ...message, state:command?.state === 'outcome_unknown' ? 'outcome_unknown'
          : command && !['failed','rejected','cancelled'].includes(command.state) ? 'accepted' : 'failed', error:command?.error?.message }
        : message));
    } catch (error) {
      const message = requestMessage(error); setNotice(`图片上传失败：${message}`); setControlBusy(false);
      if (optimistic) setOptimisticMessages((current) => current.map((entry) => entry.clientUserMessageId === clientUserMessageId
        ? { ...entry, state:'failed', error:message } : entry));
    }
  }

  function interruptTurn() {
    if (!controlCatalog?.activeTurnId) return;
    void sendInput('/interrupt');
  }

  function changeSetting(capability: string, value: string) {
    if (!detail || !controlCatalog) return;
    void runControl('/v2/commands', {
      capability,
      target: { sourceId:controlCatalog.sourceId,sourceEpoch:controlCatalog.sourceEpoch,threadKey:detail.thread.threadKey,
        codexThreadId:detail.thread.codexThreadId,expectedTurnId:null,expectedRequestId:null,expectedRequestVersion:null },
      input:{ value }
    });
  }

  async function selectNewThreadSource(sourceId: string) {
    if (!api) return;
    setNewThreadError(''); setNewThreadCatalog(undefined);
    try {
      const catalog = await api.get<ControlCatalog>(`/v2/control/catalog?sourceId=${encodeURIComponent(sourceId)}`);
      setNewThreadCatalog(catalog.data);
    } catch (error) { setNewThreadError(requestMessage(error)); }
  }

  async function openNewThread() {
    if (!api) return;
    setNewThreadOpen(true); setNewThreadError(''); setNewThreadCatalog(undefined);
    try {
      const response = await api.get<ControllerSource[]>('/v2/control/sources');
      const ready = response.data.filter((source) => source.state === 'ready'); setNewThreadSources(ready);
      if (ready[0]) await selectNewThreadSource(ready[0].sourceId);
      else setNewThreadError('Controller 已关闭或没有 ready source');
    } catch (error) { setNewThreadError(requestMessage(error)); }
  }

  async function createNewThread(input: { sourceId: string; sourceEpoch: string; cwd: string; model: string | null; personality: string | null; permissions: string | null }) {
    if (!api) return;
    setControlBusy(true); setNewThreadError('');
    try {
      const response = await api.post<GatewayCommand>('/v2/threads', input, crypto.randomUUID());
      setLatestCommand(response.data);
      setNotice(commandNotice(response.data));
      if (response.data.state !== 'completed') return;
      const threadId = record(response.data.result).threadId;
      const dashboard = await loadDashboard(api); setThreads(dashboard.threads.data); setProjects(dashboard.projects.data);
      const created = dashboard.threads.data.find((thread) => thread.codexThreadId === threadId);
      setNewThreadOpen(false);
      if (created) await selectThread(created.threadKey);
    } catch (error) { setNewThreadError(requestMessage(error)); }
    finally { setControlBusy(false); }
  }

  function respondToRequest(request: PendingRequest, action: Record<string, unknown>) {
    void runControl(`/v2/requests/${encodeURIComponent(request.requestKey)}/actions`, {
      sourceEpoch: request.sourceEpoch, expectedRequestVersion: request.requestVersion, action
    });
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

  if (!api) return <main><header class="topbar"><div class="brand"><span class="brand-mark"><Icon name="codex" size={18} /></span><h1>Codex Observer</h1><span class="readonly-label">待认证</span></div></header>
    <AuthPanel onConnect={handleConnect} error={authError} checking={authChecking} /></main>;

  return <main><header class="topbar"><div class="brand"><span class="brand-mark"><Icon name="codex" size={18} /></span><h1>Codex Observer</h1>
    <span class="readonly-label">{health?.control?.enabled ? 'V2 控制' : '只读'}</span></div>
    {health?.control?.enabled && <button type="button" class="new-thread-button" onClick={() => void openNewThread()}>新建对话</button>}
    <div class="status-stack"><div class={`health health-${health?.status || 'loading'}`} title={`后端 ${health?.status || 'loading'} · event ${health?.asOfEventSeq || 0}`}>
      <span class="status-dot" />本机数据</div><div class={`transport transport-${transport}`}>{transportText}</div></div></header>
    {dashboardState.phase === 'loading' && !health && <div class="page-status" role="status">正在加载 Dashboard…</div>}
    {dashboardState.phase === 'error' && <ErrorNotice message={`Dashboard：${dashboardState.message}`} onRetry={() => setApi(connect(token))}
      onClose={() => setDashboardState({ phase: 'idle' })} />}
    {tailscaleViewer && health?.control?.tailscaleMutationAccess && <div class="control-risk" role="alert">Tailscale 风险：当前经验证身份拥有与本机登录相同的 V2 mutation 权限。</div>}
    {notice && <div class={`notice ${notice.startsWith('已复制') ? 'notice-success' : 'notice-error'}`} role="status"><span>{notice}</span>
      <button type="button" onClick={() => setNotice('')}>关闭</button></div>}
    {newThreadOpen && <NewThreadDialog sources={newThreadSources} catalog={newThreadCatalog} busy={controlBusy} error={newThreadError}
      onSource={(sourceId) => void selectNewThreadSource(sourceId)} onClose={() => setNewThreadOpen(false)} onCreate={(input) => void createNewThread(input)} />}
    <div class={`workspace${selected ? ' detail-active' : ''}`}><Sidebar health={health} projects={projects} threads={filteredThreads} sources={sources}
      filters={filters} setFilters={(value) => { setFilters(value); if (!value.q) setSearchResults(null); }} searchResults={visibleSearchResults}
      searchState={searchState} selected={selected} onSearch={handleSearch}
      onSearchResult={(result) => selectThread(result.threadKey, { turnId: result.turnId, itemId: result.itemId })} onSelect={(key) => selectThread(key)} />
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
          catalog={controlCatalog} controlBusy={controlBusy} controlError={controlError} onSend={sendInput} onInterrupt={interruptTurn}
          latestCommand={latestCommand} optimisticMessages={optimisticMessages.filter((message) => message.threadKey === detail.thread.threadKey)} onSetting={changeSetting}
          onRequestAction={respondToRequest} />}
      </div></section>
    </div>
  </main>;
}
