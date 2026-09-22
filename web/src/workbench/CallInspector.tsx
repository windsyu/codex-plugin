import { useEffect, useLayoutEffect, useRef } from 'preact/hooks';
import type { ReadingView, ResponseView } from './reading';
import { itemRequestId, modelItems, toolItems } from './viewItems';
import { ModelMessage } from './ModelMessage';
import { RequestDetails } from './RequestDetails';
import { NativeCommandDetails, NativeFileChangeDetails, ToolCard, ToolContextDetails } from './ToolCard';

export interface SavedCallSource { epoch: string; reading: ReadingView; before?: number | null }
export type OpenCalls = (requestId: string | null, source?: SavedCallSource) => void;
export const callRequestIds = (reading: ReadingView) => [...new Set([
  ...reading.requests.map(r => r.requestId), ...reading.responses.map(r => r.requestId),
  ...reading.items.map(itemRequestId).filter((id): id is string => id !== null),
  ...reading.toolContexts.map(c => c.requestId), ...reading.diagnostics.map(d => d.requestId)
])];
const purposes = { conversation: '对话', auxiliary: '辅助', unknown: '用途未确认' };
function tokens(response: ResponseView) {
  const usage = response.usage;
  if (usage?.invalid || response.usageConflict) return '用量待核对';
  const total = usage?.totalTokens ?? (usage?.inputTokens != null && usage.outputTokens != null ? usage.inputTokens + usage.outputTokens : null);
  return total == null || !Number.isSafeInteger(total) ? 'Token 未提供' : `${total.toLocaleString('zh-CN')} Token`;
}
function status(response: ResponseView, historical: boolean) {
  return ({ receiving: historical ? '未观察到结束' : '正在接收', completed: '响应结束', failed: '响应失败', incomplete: '响应不完整' } as Record<string, string>)[response.status] || '状态未确认';
}

export function CallInspector({ reading, epoch, historical = false, before, requestId, onSelect, onClose, blocked = false }: {
  reading: ReadingView; epoch: string; historical?: boolean; before?: number | null;
  requestId: string | null; onSelect: (id: string | null) => void; onClose: () => void;
  blocked?: boolean;
}) {
  const close = useRef<HTMLButtonElement>(null);
  const body = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => { close.current?.focus({ preventScroll: true }); }, []);
  useEffect(() => { if (body.current) body.current.scrollTop = 0; }, [epoch, before, requestId]);
  const ids = callRequestIds(reading);
  const selected = reading.requests.find(r => r.requestId === requestId && r.clientRequestIndex === null);
  const unassignedModels = selected?.purpose === 'conversation' ? [] : modelItems(reading).filter(i => i.key.requestId === requestId);
  const unassignedTools = selected?.purpose === 'conversation' ? [] : toolItems(reading).filter(i => i.key.requestId === requestId);
  const scope = historical ? `历史运行 · ${epoch.slice(0, 8)}${before != null ? ' · 较早窗口' : ''}` : '本次运行';
  const title = requestId === null ? '调用记录' : '调用详情';
  return <section className="wb-call-inspector" inert={blocked} aria-label={title} data-purpose={selected?.purpose || 'unknown'} data-request-id={requestId ?? undefined} onKeyDown={event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onClose(); }
  }}>
    <div className="wb-panel-heading"><strong>{title}</strong><span className="wb-subtle">{scope}</span><button ref={close} onClick={onClose} aria-label="关闭调用面板">×</button></div>
    <div className="wb-call-body" ref={body}>
      {requestId === null ? <>
        <p className="wb-subtle">{historical ? '此保存窗口内的调用，状态固定在保存时。' : '包含对话、辅助及用途未确认的请求。较早详情可能已移出预览，累计用量仍保留。'}时长仅为单次响应的观察时长。</p>
        {!ids.length && <p className="wb-subtle">尚无可查看的调用记录。</p>}
        <div className="wb-call-list">{ids.map(id => {
          const metadata = reading.requests.filter(r => r.requestId === id);
          const request = metadata.find(r => r.clientRequestIndex === null);
          const responses = reading.responses.filter(r => r.requestId === id);
          const reported = [...new Set(responses.flatMap(r => r.reportedModels))];
          const model = reported.length ? reported.join(' / ') : request?.requestedModel || '模型未确认';
          const purpose = request?.purpose || 'unknown';
          return <button className="wb-call-row" data-request-id={id} data-purpose={purpose} key={id} onClick={() => { onSelect(id); close.current?.focus(); }}>
            <span className="wb-call-primary"><strong>{model}</strong><span className={`wb-call-purpose is-${purpose}`}>{purposes[purpose]}</span><span aria-hidden="true">›</span></span>
            <span className="wb-call-id">{id.slice(0, 8)} · {reported.length ? '响应报告' : '请求模型'}{metadata.some(m => m.clientRequestIndex !== null) ? ' · WebSocket，关联未确认' : ''}</span>
            {responses.length ? responses.map(response => <span className="wb-call-metrics" key={response.responseId || 'unknown'}>
              {responses.length > 1 && <span className="wb-call-response-id">响应 {response.responseId || '身份未确认'}</span>}
              <span>{tokens(response)}</span><span>{historical && '保存时：'}{status(response, historical)}</span><span>{response.observedDurationMs == null ? '时长未提供' : `${(response.observedDurationMs / 1000).toFixed(2)} 秒`}</span>
            </span>) : <span className="wb-call-metrics">尚未观察到响应 · 用量和时长未提供</span>}
          </button>;
        })}</div>
      </> : <>
        <button className="wb-call-back" onClick={() => { onSelect(null); close.current?.focus(); }}>← 全部调用</button>
        <p className="wb-subtle">{purposes[selected?.purpose || 'unknown']} · 请求 <code>{requestId}</code></p>
        {reading.requests.filter(r => r.requestId === requestId).map(meta => <p className="wb-subtle" key={meta.clientRequestIndex ?? 'http'}>请求模型：{meta.requestedModel || '未确认'}{meta.clientRequestIndex !== null ? ` · WebSocket create ${meta.clientRequestIndex}，响应关联未确认` : ''}</p>)}
        {reading.items.filter(item => item.kind === 'notice' && itemRequestId(item) === requestId).map(item => item.kind === 'notice' && <p key={item.itemKey} className="wb-notice" role="status">{item.text}</p>)}
        {!!(unassignedModels.length + unassignedTools.length) && <details key={requestId} className="wb-call-unassigned"><summary>未进入对话的内容（{unassignedModels.length + unassignedTools.length}）</summary>
          <p className="wb-subtle">辅助或关联未确认的内容，仅在此处阅读。</p>
          {unassignedModels.map(item => <ModelMessage key={item.itemKey} item={item} historical={historical} request={selected} response={reading.responses.find(r => r.requestId === requestId && r.responseId === item.key.responseId)} />)}
          {unassignedTools.map(tool => <ToolCard key={tool.itemKey} tool={tool} historical={historical} />)}
        </details>}
        <RequestDetails epoch={epoch} requestId={requestId} responses={reading.responses.filter(r => r.requestId === requestId)} expanded historical={historical} before={before} />
        <div className="wb-call-evidence">
          {reading.toolContexts.filter(c => c.requestId === requestId).map(context => <div key={context.clientRequestIndex ?? 'http'}>{context.clientRequestIndex !== null && <p className="wb-subtle">WebSocket create {context.clientRequestIndex} · 响应关联未确认</p>}<ToolContextDetails context={context} /></div>)}
          <NativeCommandDetails historical={historical} commands={reading.nativeCommands.filter(c => !!selected?.codexThreadId && !!selected.codexTurnId && c.key.codexThreadId === selected.codexThreadId && c.key.codexTurnId === selected.codexTurnId)} />
          <NativeFileChangeDetails changes={reading.nativeFileChanges.filter(c => !!selected?.codexThreadId && !!selected.codexTurnId && c.key.codexThreadId === selected.codexThreadId && c.key.codexTurnId === selected.codexTurnId)} />
        </div>
      </>}
    </div>
  </section>;
}
