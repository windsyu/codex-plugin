import type { ReadingView, RecorderStatus } from './reading';
import { saveLabel } from './RecorderStatus';
import { compactTokens, fullTokens, usagePartial, type UsageMetric, type UsageSummary } from './usage';

export function saveSummary(status: RecorderStatus | undefined, viewSeq: number): string {
  if (!status || status.state === 'disabled') return '仅本次可见';
  if (status.state === 'degraded') return '保存异常';
  if (status.gapCount) return '历史有缺失';
  return saveLabel(status, viewSeq);
}
export function UsageFooter({ reading, expanded, onToggle }: { reading: ReadingView; expanded: boolean; onToggle: () => void }) {
  const summary = reading.usageSummary;
  const warning = reading.recorderStatus?.state === 'degraded' || !!reading.recorderStatus?.gapCount;
  return <footer className="wb-footer">
    <button className="wb-usage-toggle" aria-label="查看用量概览" aria-expanded={expanded} aria-controls="wb-run-overview" onClick={onToggle}>
      <span className="wb-usage-footer-total" title={`已知 Token ${fullTokens(summary?.totalTokens)}`}>Token <strong>{compactTokens(summary?.totalTokens)}</strong></span>
      <span title={`输入 ${fullTokens(summary?.inputTokens)}`}>输入 {compactTokens(summary?.inputTokens)}</span>
      <span title={`输出 ${fullTokens(summary?.outputTokens)}`}>输出 {compactTokens(summary?.outputTokens)}</span>
      <span className="wb-usage-footer-cache" title={`缓存命中 ${fullTokens(summary?.cachedInputTokens)}`}>缓存 {compactTokens(summary?.cachedInputTokens)}</span>
      {summary && usagePartial(summary) && <span className="wb-usage-partial">部分用量</span>}
      <span aria-hidden="true">{expanded ? '⌄' : '⌃'}</span>
    </button>
    <button className={`wb-save-summary ${warning ? 'is-warning' : ''}`} aria-label={`保存状态：${saveSummary(reading.recorderStatus, reading.viewSeq)}`} aria-expanded={expanded} aria-controls="wb-run-overview" onClick={onToggle}>{saveSummary(reading.recorderStatus, reading.viewSeq)}</button>
    <span className="wb-footer-scope">本次运行{!reading.connected ? ' · 连接中，用量待更新' : ''}</span>
  </footer>;
}

function Metric({ label, metric, note, responses }: { label: string; metric?: UsageMetric; note?: string; responses: number }) {
  const coverage = metric?.responses || 0;
  return <div className="wb-usage-metric" aria-label={label}>
    <span>{label}</span><strong>{fullTokens(metric)}</strong>
    <small>{metric?.tokens == null ? (coverage ? '超出可显示范围' : '尚未提供') : coverage < responses ? `来自 ${coverage} / ${responses} 次响应` : note || 'Token'}</small>
  </div>;
}
export function UsageOverview({ summary, connected }: { summary?: UsageSummary; connected: boolean }) {
  const count = summary?.responseCount || 0;
  const known = summary?.totalTokens.responses || 0;
  return <section className="wb-usage-overview" aria-label="本次运行用量">
    <div className="wb-usage-scope"><span>本次运行</span><span>{count} 次模型响应</span></div>
    <div className="wb-usage-total"><span>累计已知 Token</span><strong>{fullTokens(summary?.totalTokens)}</strong></div>
    <p className="wb-usage-coverage">{!count ? '等待模型返回用量' : `已计入 ${known} / ${count} 次响应的总用量`}{!connected && ' · 连接中，等待更新'}</p>
    <div className="wb-usage-grid">
      <Metric label="输入 Token" metric={summary?.inputTokens} responses={count} />
      <Metric label="输出 Token" metric={summary?.outputTokens} responses={count} />
      <Metric label="缓存命中" metric={summary?.cachedInputTokens} responses={count} note="包含在输入中" />
      <Metric label="推理 Token" metric={summary?.reasoningTokens} responses={count} note="包含在输出中" />
    </div>
    {!!summary?.cacheWriteTokens.responses && <p className="wb-usage-extra">缓存写入 <strong>{fullTokens(summary.cacheWriteTokens)}</strong> Token · {summary.cacheWriteTokens.responses} 次响应提供</p>}
    {!!summary?.missingResponses && <p className="wb-usage-note">{summary.missingResponses} 次响应尚未提供用量，收到后自动更新。</p>}
    {!!summary?.excludedResponses && <p className="wb-usage-note is-warning">{summary.excludedResponses} 次响应的用量异常或有冲突，未计入合计。</p>}
    {summary && (summary.captureIncomplete || summary.unidentifiedResponse) && <p className="wb-usage-note">部分用量可能未收到，以上仅为已知用量。</p>}
    {summary?.capacityExceeded && <p className="wb-usage-note is-warning">本次运行已达统计上限，后续新响应未计入。</p>}
    {summary?.totalTokens.tokens === null && !!known && <p className="wb-usage-note is-warning">总量超出可显示范围，请查看各次响应用量。</p>}
    <p className="wb-usage-footnote">包含本次运行的对话和辅助请求，不含启动前的历史会话。缓存、推理明细不再叠加到总量。</p>
  </section>;
}

export function SaveOverview({ reading }: { reading: ReadingView }) {
  const status = reading.recorderStatus;
  return <div className="wb-save-overview">
    <span>对话保存</span><strong className={status?.state === 'degraded' || status?.gapCount ? 'is-warning' : ''}>{saveSummary(status, reading.viewSeq)}</strong>
    {(!status || status.state === 'disabled') && <p>退出后无法在工作台恢复本次内容。</p>}
    {status?.state === 'degraded' && <p role="status">保存暂时失败，终端仍可使用。尚未保存的内容可能在退出后丢失。</p>}
    {!!status?.gapCount && <p>部分历史未能保存；后续内容继续记录，缺失的中间过程没有被补齐。</p>}
    {reading.capture === 'partial' && <p>部分内容未能完整显示；已保存的内容也可能不完整。</p>}
  </div>;
}
