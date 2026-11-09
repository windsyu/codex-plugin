import { contextIssueLabel, contextLabels, type ContextKind, type ContextPresentation } from './contextSummary';

const number = (value: number) => value.toLocaleString('zh-CN');
const percent = (value: number, total: number) => total > 0 ? `${(value / total * 100).toFixed(1)}%` : '0.0%';

export function ContextSummaryCard({ summary, category, item, onCategory, onItem }: {
  summary: ContextPresentation; category?: ContextKind; item?: string;
  onCategory: (kind?: ContextKind) => void; onItem: (id: string) => void;
}) {
  const items = category ? summary.items.filter(value => value.kind === category) : summary.items;
  const issues = [...new Set(summary.coverage.issues.map(contextIssueLabel))];
  return <section className="wb-context-summary" aria-label="上下文分类摘要">
    <div className="wb-context-summary-heading"><strong>上下文分类</strong><span>{number(summary.tokens)} token</span></div>
    <p className="wb-subtle">{summary.calibrated ? '按上报输入量校准的估算' : '独立估算'} · {number(summary.contentBytes)} 字节。占比基于此请求已分类内容。</p>
    {!summary.coverage.complete && <p className="wb-notice">覆盖不完整，仅统计已分类内容。{issues.join('；')}{issues.length > 0 ? '。' : ''}</p>}
    <div className="wb-context-stack" aria-hidden="true">{summary.parts.map(part => <span key={part.kind} className={`is-${part.kind}`} style={{ width: percent(part.tokens, summary.tokens) }} />)}</div>
    <div className="wb-context-legend" aria-label="上下文分类占比">{summary.parts.map(part => <button key={part.kind} className={`wb-context-part is-${part.kind}`} aria-pressed={category === part.kind} onClick={() => onCategory(part.kind)}>
      <span className="wb-context-part-name"><i aria-hidden="true" />{contextLabels[part.kind]}</span><span>{number(part.tokens)} token · {percent(part.tokens, summary.tokens)}</span><small>{number(part.itemCount)} 项</small>
    </button>)}</div>
    <div className="wb-context-summary-heading"><strong>{category ? contextLabels[category] : '全部分类'}贡献项</strong>{category && <button onClick={() => onCategory()}>全部分类</button>}</div>
    {items.length === 0 ? <p className="wb-subtle">此分类没有已捕获的贡献项。</p> : <ul className="wb-context-items">{items.map(value => <li key={value.id}>
      <button aria-pressed={item === value.id} onClick={() => onItem(value.id)}><span className="wb-context-item-name">{value.name || contextLabels[value.kind]}</span><span>{number(value.tokens)} token · {percent(value.tokens, summary.tokens)}</span><small>{contextLabels[value.kind]}{value.tag ? ` · ${value.tag}` : ''} · {number(value.occurrences)} 次{value.referencesPartial ? ' · 引用不完整' : ''}</small></button>
    </li>)}</ul>}
  </section>;
}
