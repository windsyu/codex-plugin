import { browserId } from './browserId';
import { useEffect, useRef, useState } from 'preact/hooks';
import { runApi } from './runApi';

export interface StorageSize { bytes: number | null; knownBytes: number; status: string; measuredAt: string }
export interface StorageRun { runEpoch: string; startedAt: string; endedAt: string | null; state: string; size: StorageSize; manualEligible: boolean; reason: string | null; retentionReason: string | null }
interface Usage { scanId: string; state: string; runCount: number; historyBytes: number; activeBytes: number; unknownRuns: number; unverifiedEntries: number; examinedEntries: number; measuredAt: string; pendingCleanup: StorageSize; sharedManagement: StorageSize; retention?: { enabled: boolean; days: number; state: string; lastError?: string | null; lastCheckAt: string | null; nextCheckAt: string | null; scanComplete?: boolean | null; skippedCounts: Record<string, number> } }
export const storageReason: Record<string, string> = {
  current_run: '当前运行受保护', active_run: '仍有工作台在写入', unsafe_or_unreadable_entry: '文件无法安全验证', identity_changed_or_unreadable: '记录已变化或不可读取',
  not_found: '记录不存在或不属于此项目', not_found_or_unverifiable: '记录不存在或无法确认身份', ended_at_unknown: '没有可验证的结束时间', lifecycle_invalid: '结束凭证无法验证',
  unclean_run: '未正常结束', clock_invalid: '结束日期异常', not_expired: '尚未到期', unverified_entry: '部分目录无法验证', config_changed: '配置已变化，请重新预览',
  config_unavailable: '配置无法读取，清理已暂停', history_unavailable: '历史暂不可读取', history_busy_or_unavailable: '历史正在处理或无法读取'
};
Object.assign(storageReason, { preview_changed: '记录在预览后发生变化，已跳过', active_or_unsafe_run: '仍在写入或无法安全验证', storage_or_identity_error: '文件操作未完成，记录或磁盘需要核对', identity_conflict: '源目录与暂存目录冲突，已停止处理', history_missing: '无法确认记录的保存位置', cancelled: '已取消', cleanup_disabled: '历史删除已关闭，待处理项已暂停' });
storageReason.clock_changed = '检测到系统时钟变化，自动清理已暂停；请核对时间后重新启动工作台。';
export const bytesLabel = (bytes: number) => bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
export function sizeLabel(size?: StorageSize | null) {
  if (!size) return '统计中';
  if (size.bytes != null) return `约 ${bytesLabel(size.bytes)}`;
  return size.knownBytes > 0 ? `部分统计 · 至少 ${bytesLabel(size.knownBytes)}` : '占用无法完整统计';
}
export async function managementRequest<T>(epoch: string, path: string, signal: AbortSignal, body?: unknown): Promise<T> {
  const response = await fetch(runApi(`/history/${path}`), { credentials: 'same-origin', cache: 'no-store', signal,
    ...(body == null ? {} : { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ currentRunEpoch: epoch, ...body as object }) }) });
  if (!response.ok) throw new Error(({ 401: '连接验证已失效，请重新打开工作台。', 403: '历史删除未开启。', 409: '配置或历史已变化，请重新预览。', 413: '每批最多选择 100 次运行。', 503: '历史正在处理或暂不可读取，请稍后刷新。' } as Record<number, string>)[response.status] || '历史管理操作未成功，请重试。');
  const result = await response.json();
  if (result.currentRunEpoch !== epoch || !result.result) throw new Error('当前运行已改变，请重新打开工作台。');
  return result.result;
}
export function HistoryStorage({ epoch, active, onMeasured }: { epoch: string; active: boolean; onMeasured?: () => void }) {
  const [usage, setUsage] = useState<Usage | null>(null), [error, setError] = useState(''), [refresh, setRefresh] = useState(0);
  const partial = usage && (usage.state !== 'complete' || usage.unknownRuns + usage.unverifiedEntries > 0);
  const measured = useRef(onMeasured); measured.current = onMeasured;
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController(); let timer = 0, notifiedScan = '';
    const read = async () => {
      try {
        const value = await managementRequest<Usage>(epoch, 'usage', controller.signal);
        if (controller.signal.aborted) return;
        setUsage(value); setError('');
        const checking = value.state === 'scanning' || ['scheduled', 'checking'].includes(value.retention?.state || '');
        timer = window.setTimeout(() => void read(), checking ? 1000 : 5000);
        if (!checking && notifiedScan !== value.scanId) { notifiedScan = value.scanId; measured.current?.(); }
      } catch (error) { if (!controller.signal.aborted) { setError((error as Error).message); timer = window.setTimeout(() => void read(), 5000); } }
    };
    void (async () => { try { await managementRequest(epoch, 'usage/refresh', controller.signal, {}); } catch (error) { if (!controller.signal.aborted) setError((error as Error).message); } if (!controller.signal.aborted) { timer = window.setTimeout(() => void read(), 150); } })();
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [epoch, active, refresh]);
  return <div className="wb-history-storage" aria-label="当前项目历史占用">
    <div className="wb-history-storage-summary"><strong>{usage ? `当前项目：${usage.runCount} 次运行 · 历史${partial ? '已统计' : '约'} ${bytesLabel(usage.historyBytes)}` : '正在统计历史占用…'}</strong>
      {partial && <span className="is-warning">{usage.state === 'scanning' ? '统计中' : '部分统计'}</span>}
    </div>
    {usage && <>
      {(usage.pendingCleanup.bytes == null || usage.pendingCleanup.bytes > 0) && <p className="is-warning">尚未完成的删除仍占用 {sizeLabel(usage.pendingCleanup)}</p>}
      <details className="wb-storage-details"><summary>占用详情</summary>
        <p>运行中的记录约 {bytesLabel(usage.activeBytes)}，暂不删除。</p>
        <p className="wb-subtle">{usage.state === 'scanning' ? `统计中，已检查 ${usage.examinedEntries} 个目录；当前为部分结果。` : usage.state !== 'complete' ? '部分统计，不能确定完整占用。' : '按文件大小估算，实际释放的磁盘空间可能不同。'}{!!(usage.unknownRuns + usage.unverifiedEntries) && ' 有文件无法完整验证。'}</p>
        <p className="wb-subtle">更新于 {new Date(usage.measuredAt).toLocaleString('zh-CN')}</p>
        <p className="wb-subtle">索引等共享数据：{sizeLabel(usage.sharedManagement)}，未重复计入运行记录。</p>
        <button type="button" onClick={() => setRefresh(n => n + 1)}>重新统计</button>
      </details>
      {usage.retention?.enabled && <div className="wb-retention-status"><p className="wb-subtle">自动保留 {usage.retention.days} 天 · {({ scheduled: '即将检查', checking: '正在检查', waiting: '等待下次检查', clock_changed: '时钟变化，已暂停', check_failed: '检查未完成，将重试' } as Record<string, string>)[usage.retention.state] || '状态未确认'}</p>{usage.retention.lastError && <p className="wb-notice">{storageReason[usage.retention.lastError] || '检查暂不可用'}</p>}{Object.keys(usage.retention.skippedCounts).length > 0 && <details><summary>自动清理详情</summary>{usage.retention.nextCheckAt && <p className="wb-subtle">下次检查 {new Date(usage.retention.nextCheckAt).toLocaleString('zh-CN')}</p>}{Object.entries(usage.retention.skippedCounts).map(([code, count]) => <p className="wb-subtle" key={code}>{storageReason[code] || '清理条件无法确认'}：{count} 次</p>)}</details>}</div>}
      {usage.retention?.state === 'config_unavailable' && <p className="wb-notice">配置无法读取，自动清理已暂停。请修复配置后重新加载。</p>}
      {usage.retention?.enabled && usage.retention.scanComplete === false && <p className="wb-subtle">本次已检查 100 条，其余记录将在后续继续检查。</p>}
    </>}
    {error && <p className="wb-notice" role="status">{error}</p>}
    {!usage && <button type="button" onClick={() => setRefresh(n => n + 1)}>重新统计</button>}
  </div>;
}

export interface CleanupPreview { previewId: string; status: string; configRevision: string | null; expiresAt: string; mode: string; executable: boolean; items: { runEpoch: string; eligible: boolean; reason: string | null; run: StorageRun | null }[]; scanComplete: boolean; error: string | null; skippedCounts: Record<string, number> }
export function CleanupPreviewPanel({ epoch, selection, retentionDays, onClose, onChanged, onSettings }: { epoch: string; selection?: string[]; retentionDays?: number; onClose: () => void; onChanged?: () => void; onSettings?: () => void }) {
  const [preview, setPreview] = useState<CleanupPreview | null>(null), [error, setError] = useState('');
  const [submitting, setSubmitting] = useState(false), [operation, setOperation] = useState<string | null>(null);
  const submitController = useRef<AbortController | null>(null);
  const operationRef = useRef<string | null>(null);
  useEffect(() => () => submitController.current?.abort(), []);
  useEffect(() => {
    submitController.current?.abort(); operationRef.current = null; setOperation(null); setSubmitting(false); setPreview(null); setError('');
    const controller = new AbortController(); let timer = 0;
    const read = async (id: string) => {
      try {
        const value = await managementRequest<CleanupPreview>(epoch, `cleanup/previews/${encodeURIComponent(id)}`, controller.signal);
        if (controller.signal.aborted) return;
        setPreview(value);
        if (value.status === 'queued' || value.status === 'scanning') timer = window.setTimeout(() => void read(id), 400);
      } catch (error) { if (!controller.signal.aborted) setError((error as Error).message); }
    };
    void managementRequest<{ previewId: string }>(epoch, 'cleanup/preview', controller.signal, selection ? { runEpochs: selection } : { mode: 'retention', days: retentionDays }).then(value => read(value.previewId)).catch(error => { if (!controller.signal.aborted) setError((error as Error).message); });
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [epoch, selection, retentionDays]);
  const eligible = preview?.items.filter(i => i.eligible) || [];
  const knownBytes = eligible.reduce((sum, item) => sum + (item.run?.size.bytes ?? item.run?.size.knownBytes ?? 0), 0);
  const partialSize = eligible.some(item => item.run?.size.bytes == null);
  async function confirm() {
    if (!preview?.executable || !preview.configRevision || !eligible.length || submitting || operationRef.current) return;
    const id = browserId(); const controller = new AbortController(); submitController.current = controller;
    operationRef.current = id; setSubmitting(true); setError(''); setOperation(id);
    try { await managementRequest(epoch, 'cleanup/jobs', controller.signal, { previewId: preview.previewId, configRevision: preview.configRevision, operationId: id }); }
    catch (error) { if (!controller.signal.aborted) setError(`${(error as Error).message} 将继续查询本次删除结果，不会自动重发删除。`); }
    finally { if (!controller.signal.aborted) setSubmitting(false); }
  }
  return <section className="wb-cleanup-preview" aria-label={selection ? '删除确认' : '到期记录预览'}>
    <div className="wb-cleanup-heading"><strong>{operation ? '删除进度与结果' : selection ? '删除这些历史记录？' : '到期记录预览'}</strong><button type="button" autoFocus onClick={onClose}>{operation ? '关闭' : selection ? '取消' : '关闭预览'}</button></div>
    <p className="wb-subtle">仅删除本项目的工作台记录，保留 Codex 原生会话和项目文件。</p>
    {(!preview || ['queued', 'scanning'].includes(preview.status)) && !error && <p role="status">正在检查所选记录…</p>}
    {preview?.status === 'ready' && !operation && <div className="wb-cleanup-summary"><strong>{eligible.length} 条可删除</strong><span>{partialSize ? knownBytes ? `至少 ${bytesLabel(knownBytes)}，部分占用未知` : '占用暂无法完整统计' : `预计移除 ${bytesLabel(knownBytes)}`}</span></div>}
    {!!preview?.items.length && !operation && <ul className="wb-cleanup-candidates">{preview.items.map(item => <li key={item.runEpoch}><div><strong>{item.run ? new Date(item.run.startedAt).toLocaleString('zh-CN') : '无法读取的记录'}</strong><span className="wb-subtle">{item.runEpoch.slice(0, 8)}{item.run && ` · ${sizeLabel(item.run.size)}`}</span></div><span className={item.eligible ? 'wb-subtle' : 'is-warning'}>{item.eligible ? item.run?.state === 'unclean' ? '未正常结束，可能有未保存内容' : '已结束' : `保留 · ${storageReason[item.reason || ''] || '暂不能删除'}`}</span></li>)}</ul>}
    {!operation && Object.entries(preview?.skippedCounts || {}).map(([code, count]) => <p key={code} className="wb-subtle">{storageReason[code] || '无法确认清理条件'}：{count} 条保留</p>)}
    {preview && !preview.scanComplete && preview.status === 'ready' && <p className="wb-notice">已达到本批上限，其余记录不会删除。</p>}
    {(error || preview?.error) && <p role="status" className="wb-notice">{error || storageReason[preview?.error || ''] || '检查未完成，请关闭后重试。'}</p>}
    {preview?.status === 'ready' && !operation && (preview.executable ? <div className="wb-cleanup-footer"><p className="wb-subtle">删除后无法恢复。标为“保留”的记录不会删除。</p><button className="wb-delete-button" type="button" disabled={!eligible.length || submitting} onClick={() => void confirm()}>永久删除 {eligible.length} 条记录</button></div> : <div className="wb-cleanup-disabled"><strong>{preview.mode === 'retention_draft' ? '仅预览，不会删除' : '历史删除尚未开启'}</strong><p className="wb-subtle">{preview.mode === 'retention_draft' ? '这里按草稿天数列出到期记录。自动清理以保存后的设置为准。' : '请在清理设置中开启“允许清理历史记录”并保存，再回来删除。'} 当前没有删除任何记录。</p>{preview.mode !== 'retention_draft' && onSettings && <button type="button" onClick={onSettings}>前往清理设置</button>}</div>)}
    {operation && !submitting && <CleanupJobStatus epoch={epoch} id={operation} onChanged={onChanged} />}
    {submitting && <p role="status">正在提交删除…</p>}
    {operation && <p className="wb-subtle">关闭此窗口后，已确认的删除会继续。可在历史列表下方的“删除操作记录”查看结果。</p>}
  </section>;
}

interface CleanupJob { jobId: string; status: string; mode: string; createdAt: string; cancelRequested: boolean; pauseReason?: string; items: { runEpoch: string; state: string; reason: string | null; pendingCleanup: boolean }[] }
const jobStates: Record<string, string> = { planned: '等待处理', quarantined: '正在删除', deleting: '正在删除', deleted: '已删除', skipped: '已跳过', failed: '未完成', cancelled: '已取消' };
function JobResult({ job }: { job: CleanupJob }) {
  return <div className="wb-cleanup-result"><p><strong>{({ pending: '正在删除', complete: '删除处理完成', paused: '删除已暂停', failed: '部分记录未能删除' } as Record<string, string>)[job.status] || '状态未确认'}</strong> · 已删除 {job.items.filter(item => item.state === 'deleted').length} 条 / 共 {job.items.length} 条</p>
    {job.pauseReason && <p className="wb-notice">{storageReason[job.pauseReason] || '策略尚未允许继续处理。'}</p>}
    <ul>{job.items.map(item => <li key={item.runEpoch}>{item.runEpoch.slice(0, 8)} · {jobStates[item.state] || '状态未确认'}{item.reason && ` · ${storageReason[item.reason] || '条件无法确认'}`}{item.pendingCleanup && ' · 尚未释放占用空间'}</li>)}</ul>
  </div>;
}
export function CleanupJobStatus({ epoch, id, onChanged }: { epoch: string; id: string; onChanged?: () => void }) {
  const [job, setJob] = useState<CleanupJob | null>(null), [error, setError] = useState(''), [refresh, setRefresh] = useState(0);
  const action = useRef(onChanged); action.current = onChanged;
  useEffect(() => {
    const controller = new AbortController(); let timer = 0;
    const read = async () => { try {
      const value = await managementRequest<CleanupJob>(epoch, `cleanup/jobs/${encodeURIComponent(id)}`, controller.signal);
      if (controller.signal.aborted) return;
      setJob(value); setError('');
      if (value.status === 'pending') timer = window.setTimeout(() => void read(), 500); else action.current?.();
    } catch (error) { if (!controller.signal.aborted) setError((error as Error).message); } };
    void read(); return () => { controller.abort(); window.clearTimeout(timer); };
  }, [epoch, id, refresh]);
  async function cancel() { try { await managementRequest(epoch, `cleanup/jobs/${encodeURIComponent(id)}/cancel`, new AbortController().signal, {}); setRefresh(n => n + 1); } catch (error) { setError((error as Error).message); } }
  return <div aria-label="删除进度与结果">{job ? <JobResult job={job} /> : <p>正在查询删除结果…</p>}{error && <p className="wb-notice" role="status">{error}</p>}
    <div className="wb-history-actions"><button type="button" onClick={() => setRefresh(n => n + 1)}>刷新结果</button>{job && ['pending', 'paused', 'failed'].includes(job.status) && !job.cancelRequested && <button type="button" onClick={() => void cancel()}>停止剩余删除</button>}</div>
  </div>;
}
function JobDisclosure({ epoch, job }: { epoch: string; job: CleanupJob }) {
  const [open, setOpen] = useState(false);
  return <details onToggle={event => setOpen(event.currentTarget.open)}><summary>{new Date(job.createdAt).toLocaleString('zh-CN')} · {job.mode === 'retention' ? '自动删除' : '手动删除'} · {job.items.filter(i => i.state === 'deleted').length}/{job.items.length} 已删除</summary>{open && <CleanupJobStatus epoch={epoch} id={job.jobId} />}</details>;
}
export function CleanupJobs({ epoch, active }: { epoch: string; active: boolean }) {
  const [open, setOpen] = useState(false), [jobs, setJobs] = useState<CleanupJob[]>([]), [error, setError] = useState(''), [cursor, setCursor] = useState<string | null>(null), [refresh, setRefresh] = useState(0);
  useEffect(() => {
    if (!active || !open) return;
    const controller = new AbortController(); let timer = 0;
    const read = async () => { try {
      const value = await managementRequest<{ jobs: CleanupJob[]; nextCursor: string | null }>(epoch, 'cleanup/jobs', controller.signal);
      if (controller.signal.aborted) return;
      setJobs(value.jobs); setCursor(value.nextCursor); setError('');
      if (value.jobs.some(j => j.status === 'pending')) timer = window.setTimeout(() => void read(), 1000);
    } catch (error) { if (!controller.signal.aborted) setError((error as Error).message); } };
    void read(); return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, open, epoch, refresh]);
  async function more() { try { const value = await managementRequest<{ jobs: CleanupJob[]; nextCursor: string | null }>(epoch, `cleanup/jobs?cursor=${encodeURIComponent(cursor!)}`, new AbortController().signal); setJobs(old => [...old, ...value.jobs]); setCursor(value.nextCursor); } catch (error) { setError((error as Error).message); } }
  return <div className="wb-cleanup-jobs"><button type="button" aria-expanded={open} onClick={() => setOpen(v => !v)}>删除操作记录</button>{open && <><button type="button" onClick={() => setRefresh(n => n + 1)}>刷新记录</button>{!jobs.length && !error && <p className="wb-subtle">暂无删除操作。</p>}{jobs.map(job => <JobDisclosure key={job.jobId} epoch={epoch} job={job} />)}{cursor && <button type="button" onClick={() => void more()}>加载更多操作</button>}{error && <p className="wb-notice">{error}</p>}</>}</div>;
}
