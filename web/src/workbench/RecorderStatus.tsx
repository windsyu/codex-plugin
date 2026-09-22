import type { RecorderStatus } from './reading';

export function saveLabel(status: RecorderStatus | undefined, viewSeq: number): string {
  if (!status || status.state === 'disabled') return '未启用';
  if (status.state === 'degraded') return '暂未保存';
  if (status.state === 'pending' || status.savedThroughViewSeq < viewSeq) return '正在保存';
  return status.gapCount ? '已保存 · 曾有缺口' : '已保存';
}
export function RecorderDetails({ status, viewSeq }: { status: RecorderStatus | undefined; viewSeq: number }) {
  if (!status || status.state === 'disabled') return <p>保存未启用，服务退出后内存阅读内容无法恢复。</p>;
  return <div className="wb-recorder-details">
    <p>当前运行保存：{saveLabel(status, viewSeq)}。{status.state === 'degraded' ? '终端和实时阅读仍可使用，当前未保存的内容可能在退出后丢失。' : '只有同步到磁盘的记录会计入保存水位。'}</p>
    {!!status.gapCount && <p className="wb-notice">有 {status.gapCount} 处保存缺口。记录已从新分段继续，缺失的中间过程没有被补齐。</p>}
    {status.error && <p className="wb-notice">{({ queue_full: '记录队列已满', storage_failed: '历史文件暂不可写入', sync_failed: '磁盘同步失败', shutdown_timeout: '退出保存超时' } as Record<string, string>)[status.error] || '记录器暂不可用'}；将保留已确认的保存水位。</p>}
    <dl><dt>连续保存至</dt><dd>{status.persistedThroughViewSeq}</dd><dt>最近分段保存至</dt><dd>{status.savedThroughViewSeq}</dd><dt>当前阅读位置</dt><dd>{viewSeq}</dd></dl>
    <p className="wb-subtle">历史覆盖仅针对已观察内容；策略省略、捕获缺口和原生历史中的其他内容不会因此变为完整。</p>
  </div>;
}
