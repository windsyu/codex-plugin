import { useMemo, useState } from 'preact/hooks';
import { renderMarkdown } from '../lib/markdown';
import { Icon } from './Icons';
import type { LibraryRecord } from './library';
const labels: Record<string, string> = { tool_call: '工具调用', toolCall: '工具调用', tool_output: '工具结果', command_execution: '命令执行记录', nativeCommands: '命令执行记录', file_change: '文件修改记录', nativeFileChanges: '文件修改记录', reasoning: '推理摘要', context: '会话上下文', observer_instructions: '系统说明与上下文', requests: '模型请求信息', responses: '模型响应与用量', call_detail: '模型调用详情', toolContexts: '工具上下文', history_gap: '记录缺口', error: '错误记录', usage: '用量记录', plan: '计划', sub_agent: '子代理记录', mcp_tool_call: 'MCP 工具记录' };
export function recordPresentation(record: LibraryRecord): 'conversation' | 'context' | 'activity' | 'attention' {
  // Errors/gaps cannot be hidden by a malformed or older presentation hint.
  if (record.kind === 'error' || record.kind === 'history_gap') return 'attention';
  if (record.presentation) return record.presentation;
  if (['context', 'observer_instructions', 'toolContexts'].includes(record.kind)) return 'context';
  if (['user_message', 'agent_message', 'message'].includes(record.kind) && (record.role === 'user' || record.role === 'assistant')) return 'conversation';
  return 'activity';
}
export function RecordWindow({ records, all, reveal, onInspect }: { records: LibraryRecord[]; all: boolean; reveal: boolean; onInspect: (cursor: string, button: HTMLButtonElement) => void }) {
  const groups: { type: ReturnType<typeof recordPresentation>; records: LibraryRecord[] }[] = [];
  for (const record of records) {
    const type = recordPresentation(record), last = groups[groups.length - 1];
    if (!all && (type === 'context' || type === 'activity') && last?.type === type) last.records.push(record);
    else groups.push({ type, records: [record] });
  }
  return <>{groups.map(group => all || group.type === 'conversation' || group.type === 'attention'
    ? group.records.map(record => <HistoryRecord key={record.detailCursor} record={record} onInspect={onInspect} />)
    : <RecordFold key={group.records[0].detailCursor} records={group.records} type={group.type} reveal={reveal} onInspect={onInspect} />)}</>;
}
function RecordFold({ records, type, reveal, onInspect }: { records: LibraryRecord[]; type: 'context' | 'activity'; reveal: boolean; onInspect: (cursor: string, button: HTMLButtonElement) => void }) {
  const [open, setOpen] = useState(reveal);
  const kinds = Array.from(new Set(records.map(record => type === 'context' ? '会话上下文' : labels[record.kind] || '未识别记录')));
  return <details className="wb-library-record-fold" open={open} onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>{type === 'context' ? '上下文' : '工具与过程'} · {records.length} 条<span>{kinds.slice(0, 3).join('、')}{kinds.length > 3 ? '等' : ''}</span></summary>
    {open && records.map(record => <HistoryRecord key={record.detailCursor} record={record} onInspect={onInspect} />)}
  </details>;
}
export function HistoryRecord({ record, onInspect }: { record: LibraryRecord; onInspect: (cursor: string, button: HTMLButtonElement) => void }) {
  const role = recordPresentation(record) === 'conversation' ? record.role === 'user' ? 'user' : record.role === 'assistant' ? 'assistant' : 'record' : 'record';
  const text = record.text || '';
  // Reuse the same safe Markdown renderer. History never fetches embedded images.
  // Limit each rendered Markdown fragment as well as the number of mounted records.
  const html = useMemo(() => {
    if (role !== 'assistant') return '';
    const template = document.createElement('template');
    template.innerHTML = renderMarkdown(text.slice(0, 4000));
    template.content.querySelectorAll('img,video,audio,source').forEach(n => n.remove());
    template.content.querySelectorAll('a').forEach(n => { n.setAttribute('rel', 'noreferrer noopener'); n.setAttribute('target', '_blank'); });
    return template.innerHTML;
  }, [text, role]);
  const label = role === 'user' ? '用户' : role === 'assistant' ? '模型' : recordPresentation(record) === 'context' ? '会话上下文' : labels[record.kind] || '未识别记录';
  return <article className={`wb-library-record ${role === 'user' ? 'wb-user-message' : role === 'assistant' ? 'wb-model-message' : 'wb-library-event'}`} data-record-cursor={record.detailCursor} data-role={role} data-kind={record.kind}>
    <div className="wb-message-meta"><Icon name={role === 'record' ? 'code' : 'chat'} /><strong>{label}</strong>{record.label && <code>{record.label}</code>}<button className="wb-inspect-link" onClick={e => onInspect(record.detailCursor, e.currentTarget)}>查看详情</button></div>
    {role === 'assistant' ? <div className="wb-message-body" dangerouslySetInnerHTML={{ __html: html || '未保存可展示的文本。' }} />
      : role === 'user' ? <div className="wb-user-body">{text.slice(0, 12000) || '本条记录没有可展示的文本。'}</div>
      : <><pre>{text.slice(0, 2000) || '展开详情查看此来源保存的字段。'}</pre>{record.kind === 'tool_call' && <small>调用参数不代表已执行成功。</small>}</>}
    {record.usage && <dl className="wb-library-usage">{Object.entries({ inputTokens: '输入 token', outputTokens: '输出 token', totalTokens: '总 token' }).map(([key, name]) => <div key={key}><dt>{name}</dt><dd>{record.usage?.[key] == null ? '未记录' : Number(record.usage[key]).toLocaleString('zh-CN')}</dd></div>)}</dl>}
    {(record.truncated || text.length > (role === 'assistant' ? 4000 : role === 'user' ? 12000 : 2000)) && <p className="wb-subtle">此处显示摘要，可按需查看已保存的详情。</p>}
  </article>;
}
