import { useEffect, useRef, useState } from 'preact/hooks';
import { conversationEntries, readSnapshot, type ReadingView } from './reading';
import { userMessages } from './viewItems';
import { ModelMessage } from './ModelMessage';
import { UserMessage } from './UserMessage';
import { ToolCard } from './ToolCard';
import type { OpenCalls } from './CallInspector';
import { HistoryStorage, CleanupPreviewPanel, CleanupJobs, sizeLabel, storageReason, type StorageRun } from './HistoryStorage';
import { runApi } from './runApi';

interface HistoryRun { runEpoch: string; projectName: string; startedAt: string; state: 'active' | 'ended' | 'unclean'; savedThroughViewSeq: number; persistedThroughViewSeq: number; gapCount: number; historyCoverage: string; storage?: StorageRun | null }
interface Page { currentRunEpoch: string; runs: HistoryRun[]; nextCursor: string | null; diagnostics: { runEpoch: string; code: string }[]; indexState: string }
interface SavedRun { run: HistoryRun; snapshot: unknown; uncertainTail: boolean; gaps: { afterViewSeq: number; throughViewSeq: number; reason: string }[]; issues: { segment: string; byteOffset: number; code: string }[]; detailsPartial: boolean; currentRunEpoch: string; before?: number | null; previousBefore?: number | null }
const states = { active: '运行中 · 已保存部分', ended: '正常结束', unclean: '未正常结束' };
export const historyIssue: Record<string, string> = { snapshot_invalid: '快照无法验证', journal_unavailable: '日志不可读取', committed_tail_missing: '已保存日志缺失', unsaved_tail: '存在未确认保存的尾部', partial_line: '日志尾部不完整', invalid_record: '日志记录损坏', checkpoint_mismatch: '日志与保存水位不一致' };
const failure = (status: number) => ({ 400: '历史游标无效，请刷新列表。', 401: '连接验证已失效，请重新打开工作台。', 404: '此运行没有可读取的历史记录。', 409: '历史列表或记录已更新，请刷新后重试。', 503: '历史暂不可读取，请稍后重试。' } as Record<number, string>)[status] || '历史读取失败，请重试。';

export function HistoryPanel({ epoch, active, blocked = false, managementAvailable = false, onReturn, onInspect, onSelectionChange, onSettings }: { epoch: string; active: boolean; blocked?: boolean; managementAvailable?: boolean; onReturn: () => void; onInspect?: OpenCalls; onSelectionChange?: () => void; onSettings?: () => void }) {
  const [page, setPage] = useState<Page | null>(null), [runs, setRuns] = useState<HistoryRun[]>([]);
  const [saved, setSaved] = useState<{ value: SavedRun; reading: ReadingView } | null>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState('');
  const controller = useRef<AbortController | null>(null);
  const initialized = useRef(false);
  const [selected, setSelected] = useState<string[]>([]), [previewSelection, setPreviewSelection] = useState<string[] | null>(null);
  const [storageRefresh, setStorageRefresh] = useState(0);
  const [selecting, setSelecting] = useState(false);
  const batchButton = useRef<HTMLButtonElement>(null);
  const canDelete = (run: HistoryRun) => run.runEpoch !== epoch && run.state !== 'active' && run.storage?.manualEligible !== false;
  const selectable = runs.filter(canDelete);
  const allSelected = selectable.length > 0 && selectable.every(run => selected.includes(run.runEpoch));
  function closeDeletion() { setPreviewSelection(null); requestAnimationFrame(() => batchButton.current?.focus()); }
  const selectionChanged = useRef(onSelectionChange); selectionChanged.current = onSelectionChange;
  async function load(id?: string, cursor?: string, before?: number | null) {
    controller.current?.abort(); const abort = new AbortController(); controller.current = abort;
    setBusy(true); setError('');
    try {
      const response = await fetch(runApi(`/history${id ? `/${encodeURIComponent(id)}` : ''}${before != null ? `?before=${before}` : cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`), { credentials: 'same-origin', cache: 'no-store', signal: abort.signal });
      if (!response.ok) {
        if (id && saved?.value.run.runEpoch === id && [404, 410].includes(response.status)) { setSaved(null); selectionChanged.current?.(); setRuns(old => old.filter(run => run.runEpoch !== id)); setError('此历史已删除或不存在，请刷新列表。'); return; }
        throw new Error(failure(response.status));
      }
      const value = await response.json();
      if (abort.signal.aborted) return;
      if (value.currentRunEpoch !== epoch) throw new Error('当前运行已改变，请重新打开工作台。');
      if (id) {
        if (value.run?.runEpoch !== id || !Array.isArray(value.issues) || !Array.isArray(value.gaps)) throw new Error('历史格式无法识别。');
        const reading = readSnapshot(value.snapshot, id);
        onSelectionChange?.(); setSaved({ value, reading });
      } else {
        if (!Array.isArray(value.runs) || value.runs.length > 20) throw new Error('历史格式无法识别。');
        setPage(value); setRuns(old => cursor ? [...old, ...value.runs] : value.runs);
      }
    } catch (reason) { if (!abort.signal.aborted) setError(reason instanceof Error ? reason.message : '历史暂不可读取。'); }
    finally { if (!abort.signal.aborted) setBusy(false); }
  }
  useEffect(() => { if (active && !initialized.current) { initialized.current = true; void load(); } }, [active]);
  useEffect(() => () => controller.current?.abort(), []);
  useEffect(() => {
    if (!active || !managementAvailable || !saved) return;
    const id = saved.value.run.runEpoch, abort = new AbortController(); let timer = 0;
    const check = async () => {
      try { const response = await fetch(runApi(`/history/${encodeURIComponent(id)}/status`), { credentials: 'same-origin', cache: 'no-store', signal: abort.signal });
        if (abort.signal.aborted) return;
        if ([404, 410].includes(response.status)) { controller.current?.abort(); setBusy(false); setSaved(null); selectionChanged.current?.(); setRuns(old => old.filter(run => run.runEpoch !== id)); setError('此历史已删除或不存在，请刷新列表。'); return; }
      } catch { /* Keep readable content on a transient network error. */ }
      if (!abort.signal.aborted) timer = window.setTimeout(() => void check(), 3000);
    };
    timer = window.setTimeout(() => void check(), 3000); return () => { abort.abort(); window.clearTimeout(timer); };
  }, [epoch, active, managementAvailable, saved?.value.run.runEpoch]);
  return <section className="wb-history" aria-label="历史运行" hidden={!active} inert={blocked}>
    <div className="wb-history-actions wb-history-toolbar"><button onClick={onReturn}>返回实时阅读</button><button disabled={busy} onClick={() => { void load(saved?.value.run.runEpoch); if (!saved) setStorageRefresh(n => n + 1); }}>刷新{saved ? '此记录' : '列表'}</button>{saved && <button onClick={() => { onSelectionChange?.(); setSaved(null); }}>历史列表</button>}
      {!saved && managementAvailable && <div className="wb-history-tools">{onSettings && <button onClick={onSettings}>清理设置</button>}<button ref={batchButton} aria-pressed={selecting} onClick={() => { setSelecting(!selecting); setSelected([]); }}>{selecting ? '退出选择' : '批量删除'}</button></div>}
    </div>
    {saved && <p className="wb-notice">历史内容只读。右侧终端始终属于当前运行，输入会发送给当前 CLI。</p>}
    {busy && <p role="status">正在读取已保存记录…</p>}{error && <p className="wb-notice" role="status">{error}</p>}
    {saved ? <><div className="wb-history-actions">{saved.value.previousBefore != null && <button disabled={busy} onClick={() => void load(saved.value.run.runEpoch, undefined, saved.value.previousBefore)}>查看更早的记录</button>}{saved.value.before != null && <button disabled={busy} onClick={() => void load(saved.value.run.runEpoch)}>最近保存内容</button>}</div><SavedReading key={`${saved.value.run.runEpoch}:${saved.value.before ?? ''}`} {...saved} onInspect={onInspect} /></> : <>
      <p className="wb-subtle wb-history-help">本项目的已保存对话。查看历史不会切换右侧的当前终端。</p>
      {managementAvailable && <><HistoryStorage key={storageRefresh} epoch={epoch} active={active} onMeasured={() => { if (!busy && !saved) void load(); }} />
        {selecting && <div className="wb-history-selection" aria-label="批量删除记录">
          <label><input type="checkbox" checked={allSelected} disabled={!selectable.length} onChange={event => setSelected(event.currentTarget.checked ? selectable.slice(0, 100).map(run => run.runEpoch) : [])} />全选已加载记录</label>
          <span>已选 {selected.length} 条<span className="wb-subtle"> · 最多 100 条</span></span>
          <button className="wb-delete-button" disabled={!selected.length || busy} onClick={() => setPreviewSelection([...selected])}>删除所选{selected.length ? `（${selected.length}）` : ''}</button>
        </div>}
        {previewSelection && <HistoryDeleteDialog onClose={closeDeletion}><CleanupPreviewPanel epoch={epoch} selection={previewSelection} onClose={closeDeletion} onSettings={onSettings ? () => { closeDeletion(); onSettings(); } : undefined} onChanged={() => { setStorageRefresh(n => n + 1); setSelected([]); setSelecting(false); void load(); }} /></HistoryDeleteDialog>}
      </>}
      {page && !runs.length && <p>尚无已保存的运行。</p>}
      {!!page?.diagnostics.length && <p className="wb-notice">{page.diagnostics.length} 份运行记录无法验证，未将其显示为完整历史。</p>}
      {page?.indexState === 'unavailable' && <p className="wb-notice">历史索引暂不可用，以下列表来自已保存记录。</p>}
      <div className="wb-history-list">{runs.map(run => <div className={`wb-history-row ${selected.includes(run.runEpoch) ? 'is-selected' : ''}`} key={run.runEpoch}>{managementAvailable && selecting && <input type="checkbox" aria-label={`选择记录 ${run.runEpoch.slice(0, 8)}`} disabled={!canDelete(run) || (!selected.includes(run.runEpoch) && selected.length >= 100)} checked={selected.includes(run.runEpoch)} onChange={event => setSelected(old => event.currentTarget.checked ? [...old, run.runEpoch] : old.filter(id => id !== run.runEpoch))} />}<button className="wb-history-open" disabled={busy} onClick={() => void load(run.runEpoch)}>
        <strong>{run.projectName} · {run.runEpoch.slice(0, 8)}{run.runEpoch === epoch ? ' · 当前运行' : ''}</strong>
        <span>{new Date(run.startedAt).toLocaleString('zh-CN')} · {states[run.state] || '状态未确认'}</span>
        <span>{run.gapCount ? '保存存在缺口' : '已保存观察记录'}</span>
        {managementAvailable && <span>{sizeLabel(run.storage?.size)}{!canDelete(run) && ` · ${run.runEpoch === epoch || run.state === 'active' ? '正在运行，暂不能删除' : storageReason[run.storage?.reason || ''] || '暂不能删除'}`}</span>}
      </button>{managementAvailable && !selecting && canDelete(run) && <button className="wb-history-delete" disabled={busy} aria-label={`删除记录 ${run.runEpoch.slice(0, 8)}`} onClick={() => setPreviewSelection([run.runEpoch])}><svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><path d="M2.5 4.5h11M6 4V2.5h4V4M4 4.5l.5 9h7l.5-9M6.5 6.5v5M9.5 6.5v5" /></svg>删除</button>}</div>)}</div>
      {page?.nextCursor && <button disabled={busy} onClick={() => void load(undefined, page.nextCursor!)}>加载更多运行</button>}
      {managementAvailable && <CleanupJobs key={storageRefresh} epoch={epoch} active={active} />}
    </>}
  </section>;
}
function HistoryDeleteDialog({ children, onClose }: { children: preact.ComponentChildren; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => { const element = dialog.current!; element.showModal(); return () => element.close(); }, []);
  return <dialog ref={dialog} className="wb-history-delete-dialog" aria-label="删除历史记录" onCancel={event => { event.preventDefault(); onClose(); }}>{children}</dialog>;
}
function SavedReading({ value, reading, onInspect }: { value: SavedRun; reading: ReadingView; onInspect?: OpenCalls }) {
  const entries = conversationEntries(reading), users = userMessages(reading);
  const inspect = (requestId: string | null) => onInspect?.(requestId, { epoch: value.run.runEpoch, reading, before: value.before });
  const responseFor = (item: { key: { requestId: string; responseId: string | null } }) => reading.responses.find(r => r.requestId === item.key.requestId && r.responseId === item.key.responseId);
  return <div className="wb-saved-reading">
    <h2>历史运行 · {value.run.runEpoch.slice(0, 8)}</h2><p className="wb-subtle">{states[value.run.state]} · 来源：已保存工作台记录</p><p className="wb-subtle">以下状态固定在保存时，不表示当前仍在生成或执行。</p>
    {value.uncertainTail && <p role="status" className="wb-notice">此服务未确认正常退出。曾显示但未保存的尾部可能已经丢失，无法确定丢失数量。</p>}
    {!!value.gaps.length && <p className="wb-notice">保存有 {value.gaps.length} 处缺口；恢复后的快照不代表缺失的中间过程已补齐。</p>}
    {value.detailsPartial && <p className="wb-notice">部分上下文超出保存预览范围，详情可能不完整。</p>}
    {reading.capture !== 'ok' && <p className="wb-notice">保存的观察内容存在捕获缺口、截断或省略；已保存不等于完整捕获。</p>}
    {reading.items.filter(item => item.kind === 'notice').map(item => <p key={item.itemKey} role="status" className="wb-notice">{item.text}</p>)}
    {!!value.issues.length && <ul className="wb-notice">{value.issues.map((issue, i) => <li key={i}>{historyIssue[issue.code] || '历史记录不完整'} · 分段 {issue.segment.slice(0, 8)} / {issue.byteOffset}</li>)}</ul>}
    <div className="wb-history-actions">{onInspect && <button onClick={() => inspect(null)}>查看此历史的调用记录</button>}</div>
    <div>{!entries.length && <p className="wb-subtle">此记录尚无可关联的对话，可从调用记录查看独立请求。</p>}{entries.map(entry => <div key={entry.key}>{entry.kind === 'user' ? <UserMessage user={entry.user} orderUnconfirmed={entry.orderUnconfirmed} /> : entry.kind === 'tool' ? <ToolCard historical tool={entry.tool} onInspect={onInspect ? () => inspect(entry.tool.key.requestId) : undefined} /> : <ModelMessage historical item={entry.item} response={responseFor(entry.item)} request={entry.request} onInspect={onInspect ? () => inspect(entry.item.key.requestId) : undefined} userPending={!users.some(user => user.key.codexThreadId === entry.request.codexThreadId && user.key.codexTurnId === entry.request.codexTurnId)} />}</div>)}</div>
  </div>;
}
