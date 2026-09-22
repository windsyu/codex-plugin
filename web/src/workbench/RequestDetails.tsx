import { useEffect, useRef, useState } from 'preact/hooks';
import type { ResponseView } from './reading';
import { responseLabel } from './ModelMessage';

type Source = { kind: 'request'; clientRequestIndex: number | null } | { kind: 'response'; responseId: string | null };
interface Entry { source: Source; captureSeq: number; section: string; position: number | null; jsonPointer: string; childrenSeparated: boolean; preview: string; truncated: boolean; omitted: boolean }
interface Page {
  runEpoch: string; requestId: string; revision: number; availability: 'captured' | 'pending' | 'unavailable';
  requestCaptured: boolean; responseCaptured: boolean; truncated: boolean; omitted: boolean; conflict: boolean;
  totalEntries: number; entries: Entry[]; nextCursor: string | null;
  captureIssues: string[];
}
const MAX_ENTRIES = 256;
const MAX_BYTES = 2 * 1024 * 1024;
const labels: Record<string, string> = { settings: '请求参数', instructions: '系统说明', input: '请求消息', tools: '工具定义', response: '响应信息', output: '响应输出' };
const responseVersion = (responses: ResponseView[]) => JSON.stringify(responses.map(r => [r.responseId, r.status, r.usageConflict]));
const sourceLabel = (source: Source) => source.kind === 'request'
  ? (source.clientRequestIndex === null ? 'HTTP 请求' : `WebSocket create ${source.clientRequestIndex} · 响应关联未确认`)
  : `响应 ${source.responseId || '身份未确认'}`;
const count = (value: number | null | undefined) => value == null ? '未提供' : value.toLocaleString('zh-CN');

export function ResponseUsage({ response, historical = false }: { response: ResponseView; historical?: boolean }) {
  const usage = response.usage;
  return <section className="wb-response-usage" aria-label={`响应用量 ${response.responseId || '身份未确认'}`}>
    <strong>{responseLabel(response.status, historical)} · <code>{response.responseId || '响应身份未确认'}</code></strong>
    {usage ? <dl>
      <dt>输入 token</dt><dd>{count(usage.inputTokens)}</dd><dt>缓存命中 token</dt><dd>{count(usage.cachedInputTokens)}</dd>
      <dt>输出 token</dt><dd>{count(usage.outputTokens)}</dd><dt>其中推理 token</dt><dd>{count(usage.reasoningTokens)}</dd>
      <dt>总 token</dt><dd>{count(usage.totalTokens)}</dd><dt>缓存写入 token</dt><dd>{count(usage.cacheWriteTokens)}</dd>
    </dl> : <p className="wb-subtle">尚未收到此响应的用量，缺失值不计为零。</p>}
    {usage?.invalid && <p className="wb-notice">上游用量字段异常或计数不一致，数值仅供核对。</p>}
    {response.usageConflict && <p className="wb-notice">收到冲突用量，保留首份记录；可在响应详情中核对。</p>}
    <p className="wb-subtle">响应观察时长：{response.observedDurationMs == null ? '未提供' : `${(response.observedDurationMs / 1000).toFixed(2)} 秒`}。范围为观察到响应开始至终态，不是任务总耗时。</p>
  </section>;
}

export function RequestDetails({ epoch, requestId, responses, expanded, onToggle, historical = false, before }: {
  epoch: string; requestId: string; responses: ResponseView[]; expanded: boolean; onToggle?: () => void; historical?: boolean; before?: number | null;
}) {
  return <section className="wb-request-context">
    {onToggle && <button className="wb-context-toggle" aria-expanded={expanded} onClick={onToggle}>{expanded ? '▾' : '▸'} 上下文与用量</button>}
    {expanded && <ContextBody key={`${epoch}:${requestId}:${before ?? ''}`} epoch={epoch} requestId={requestId} responses={responses} historical={historical} before={before} />}
  </section>;
}

function ContextBody({ epoch, requestId, responses, historical, before }: { epoch: string; requestId: string; responses: ResponseView[]; historical: boolean; before?: number | null }) {
  const [page, setPage] = useState<Page | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [capped, setCapped] = useState(false);
  const [version, setVersion] = useState('');
  const inFlight = useRef<AbortController | null>(null);
  const disposed = useRef(false);
  const bytes = useRef(0);
  const latestVersion = useRef(responseVersion(responses));
  latestVersion.current = responseVersion(responses);
  async function load(cursor?: string, replace = false) {
    if (inFlight.current) return;
    const controller = new AbortController(); inFlight.current = controller;
    const requestedVersion = latestVersion.current;
    setBusy(true); setError('');
    try {
      const endpoint = historical ? `/workbench/v1/history/${encodeURIComponent(epoch)}/requests/${encodeURIComponent(requestId)}` : `/workbench/v1/requests/${encodeURIComponent(requestId)}?epoch=${encodeURIComponent(epoch)}`;
      const parameters = [cursor ? `cursor=${encodeURIComponent(cursor)}` : '', historical && before != null ? `before=${before}` : ''].filter(Boolean).join('&');
      const result = await fetch(`${endpoint}${parameters ? `${historical ? '?' : '&'}${parameters}` : ''}`, { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
      if (!result.ok) throw new Error(({ 400: '详情游标无效，请刷新上下文。', 401: '连接验证已失效，请重新打开工作台。', 404: '尚未观察到此请求。', 409: '上下文或运行已更新，请刷新上下文。', 410: '此请求详情已移出内存缓存。', 503: '阅读服务繁忙，请稍后重试。' } as Record<number, string>)[result.status] || '请求详情读取失败，请重试。');
      const raw = await result.text();
      if (raw.length > 128 * 1024) throw new Error('详情页超过阅读限制。');
      const next: Page = JSON.parse(raw);
      if (next.runEpoch !== epoch || next.requestId !== requestId || !Array.isArray(next.entries) || next.entries.length > 16 || (cursor && page?.revision !== next.revision)) throw new Error('上下文或运行已更新，请刷新上下文。');
      if (disposed.current) return;
      const previous = cursor && !replace ? entries : [];
      const baseBytes = cursor && !replace ? bytes.current : 0;
      const addedBytes = new TextEncoder().encode(raw).length;
      if (previous.length + next.entries.length > MAX_ENTRIES || baseBytes + addedBytes > MAX_BYTES) {
        setCapped(true); return;
      }
      bytes.current = baseBytes + addedBytes;
      setEntries([...previous, ...next.entries]); setPage(next); setCapped(false); setVersion(requestedVersion);
    } catch (reason) {
      if (!disposed.current && !controller.signal.aborted) setError(reason instanceof SyntaxError ? '详情格式无法识别，请刷新上下文。' : (reason as Error).message === 'Failed to fetch' ? '连接暂时不可用，请重试。' : (reason as Error).message);
    } finally {
      inFlight.current = null;
      if (!disposed.current) setBusy(false);
    }
  }
  useEffect(() => {
    disposed.current = false; void load();
    return () => { disposed.current = true; inFlight.current?.abort(); };
  }, [epoch, requestId]);
  return <div className="wb-context-body">
    <p className="wb-subtle">{historical ? '按需读取此历史运行已保存的脱敏观察副本。' : '按需读取本次运行的脱敏观察副本。'}请求中的历史消息与系统上下文不代表新的用户提交；WebSocket create 与响应分别列出，不猜测对应关系。</p>
    {responses.map(response => <ResponseUsage key={response.responseId || 'unknown'} response={response} historical={historical} />)}
    <p className="wb-subtle">用量属于各次模型响应，不汇总为整个任务，也不重复累加缓存或推理明细。</p>
    <div className="wb-context-actions"><button disabled={busy} onClick={() => void load()}>刷新上下文</button>{busy && <span role="status">正在读取…</span>}</div>
    {error && <p role="status" className="wb-notice">{error}</p>}
    {page && <>
      {version !== latestVersion.current && <p className="wb-notice">已收到新的响应状态，可刷新上下文。</p>}
      {page.availability === 'pending' && <p className="wb-subtle">尚未捕获完整请求或响应文档，可稍后刷新。</p>}
      {page.availability === 'unavailable' && <p className="wb-notice">存在观察缺口，当前没有可用的上下文文档。</p>}
      {page.availability === 'captured' && <p className="wb-subtle">请求文档：{page.requestCaptured ? '已捕获' : '未捕获'} · 响应文档：{page.responseCaptured ? '已捕获' : '未捕获，可在响应结束后刷新'} · 当前显示 {entries.length} / {page.totalEntries} 项</p>}
      {!!page.captureIssues?.length && <p className="wb-notice">观察记录存在缺口或未识别内容，以下文档不代表完整上下文。</p>}
      {page.omitted && <p className="wb-subtle">认证、加密内容、媒体或部分字段已按策略省略。</p>}
      {page.truncated && <p className="wb-notice">上下文超过观察或缓存限制，仅保留部分内容。</p>}
      {page.conflict && <p className="wb-notice">同一来源出现不同内容，各份观察独立保留。</p>}
      <div className="wb-context-entries">{entries.map((entry, index) => <details className="wb-context-entry" key={`${entry.captureSeq}:${entry.section}:${index}`}>
        <summary>{labels[entry.section] || '未识别内容'}{entry.position === null ? '' : ` ${entry.position + 1}`} · {sourceLabel(entry.source)}{entry.truncated ? ' · 已截断' : ''}{entry.omitted ? ' · 有省略' : ''}</summary>
        {entry.jsonPointer && <p className="wb-subtle wb-context-location">请求或响应中的位置：<code>{entry.jsonPointer}</code></p>}
        {entry.childrenSeparated && <p className="wb-subtle wb-context-location">内含工具定义已单独列出，可继续向后阅读。</p>}
        <pre>{entry.preview}</pre>
      </details>)}</div>
      {capped ? <div className="wb-notice">当前内容已达阅读上限；继续阅读会替换当前列表。 <button disabled={busy} onClick={() => void load(page.nextCursor!, true)}>继续阅读后续页</button></div>
        : page.nextCursor && <button disabled={busy} onClick={() => void load(page.nextCursor!)}>加载后续内容</button>}
    </>}
  </div>;
}
