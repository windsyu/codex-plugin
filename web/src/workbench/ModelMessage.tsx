import { useMemo } from 'preact/hooks';
import { renderMarkdown } from '../lib/markdown';
import type { TextItem, ResponseView, RequestView } from './reading';

export const responseLabels: Record<string, string> = { receiving: '正在接收', completed: '本次模型响应已结束 · 不代表任务结束', failed: '本次响应失败', incomplete: '本次响应不完整' };

export function responseLabel(status?: string, historical = false) {
  const label = historical && status === 'receiving' ? '尚未观察到响应结束' : responseLabels[status || ''] || '正文已捕获 · 响应状态未确认';
  return `${historical ? '保存时：' : ''}${label}`;
}

export function ModelMessage({ item, response, request, userPending = false, historical = false, onInspect }: { item: TextItem; response?: ResponseView; request?: RequestView; userPending?: boolean; historical?: boolean; onInspect?: () => void }) {
  const html = useMemo(() => renderMarkdown(item.text), [item.text]);
  const reported = item.author?.reportedModels || response?.reportedModels || [];
  const requestedModel = item.author ? item.author.requestedModel : request?.requestedModel;
  const modelLabel = reported.length > 1 ? `模型名冲突：${reported.join(' / ')}`
    : reported[0] ? `${reported[0]} · 响应报告` : requestedModel ? `${requestedModel} · 请求模型` : '名称未确认';
  return <article className="wb-model-message" data-request-id={item.key.requestId} data-role="assistant">
    <div className="wb-message-meta"><span className="wb-model-avatar">✳</span><strong>模型</strong><span>{modelLabel}</span>{onInspect && <button className="wb-inspect-link" onClick={onInspect}>调用详情</button>}</div>
    {!!reported.length && requestedModel && !reported.includes(requestedModel) && <p className="wb-subtle">请求模型：{requestedModel}；与响应报告不同</p>}
    <div className="wb-message-body" dangerouslySetInnerHTML={{ __html: html }} />
    <div className="wb-message-status">{responseLabel(response?.status, historical)}{item.truncated ? ' · 正文预览已截断' : ''}</div>
    {userPending && <p className="wb-subtle" role="status">尚未取得本轮原生用户提交记录。</p>}
  </article>;
}
