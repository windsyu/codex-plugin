import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { RecordWindow, recordPresentation } from './HistoryRecord';
import './library-reader.css';
import { OrganizationEditor } from './OrganizationEditor';
import { Icon } from './Icons';
import { useLibraryState } from './libraryState';
import { coverageText, dateLabel, failure, libraryRead, sourceNames, unassignedReasonText, type HistoryRoute, type LibraryEntry, type LibraryPage, type LibraryRecord, type OrganizationResponse } from './library';
interface Block { cursor: string | null; height: number }
interface SavedPosition { entry: string; revision: string; cursor: string | null; offset: number }
export function LibraryReader({ route, onBack, onEntry, sources, onResume, instanceId, onOrganizationSaved }: { instanceId?: string; onOrganizationSaved?: (result: OrganizationResponse) => void; onResume?: (entry: LibraryEntry) => void; route: HistoryRoute; onBack: () => void; onEntry: (id: string) => void; sources: { id: string; identity?: string }[] }) {
  const state = useLibraryState();
  const saved = useRef<SavedPosition | undefined>(state.read().libraryReading?.entry === route.entry ? state.read().libraryReading : undefined);
  const [blocks, setBlocks] = useState<Block[]>([{ cursor: saved.current?.cursor || null, height: 700 }]);
  const blocksRef = useRef(blocks); blocksRef.current = blocks;
  const cache = useRef(new Map<number, LibraryPage<LibraryRecord>>());
  const [version, setVersion] = useState(0);
  const [active, setActive] = useState(0);
  const activeRef = useRef(active); activeRef.current = active;
  const [entry, setEntry] = useState<LibraryEntry>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [detail, setDetail] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [allRecords, setAllRecords] = useState(false);
  const [prefixNotice, setPrefixNotice] = useState('');
  const seeking = useRef(!saved.current && !route.match);
  const seekIndex = useRef(0);
  const pendingSeek = useRef<number | null>(null);
  const modeAnchor = useRef<{ cursor: string; top: number } | null>(null);
  const organizationButton = useRef<HTMLButtonElement>(null);
  const returnFocus = useRef<HTMLButtonElement | null>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const inflight = useRef<AbortController | null>(null);
  const disposed = useRef(false);
  const ignoreMatch = useRef(false);
  const pinned = useRef(route.revision || saved.current?.revision || '');
  const [retry, setRetry] = useState(0);
  const start = Math.max(0, active - 1), end = Math.min(blocks.length, active + 2);
  const first = cache.current.get(0) || cache.current.values().next().value;
  const revoked = !!entry && !sources.some(s => s.id === entry.sourceId && (!entry.sourceIdentity || !s.identity || entry.sourceIdentity === s.identity));
  const organizationUnavailable = entry?.organization?.overrideState === 'unavailable';
  const remember = () => {
    const top = scroll.current?.scrollTop || 0;
    let i = 0, height = 0;
    while (i + 1 < blocksRef.current.length && height + blocksRef.current[i].height < top) height += blocksRef.current[i++].height;
    state.replace({ libraryReading: { entry: route.entry, revision: pinned.current, cursor: blocksRef.current[i].cursor, offset: Math.max(0, top - height) } });
    return i;
  };
  const load = async (index: number) => {
    if (inflight.current || disposed.current || revoked) return;
    const controller = new AbortController(); inflight.current = controller; setBusy(true);
    try {
      const block = blocksRef.current[index];
      const params = new URLSearchParams({ window: 'true', limit: '16' });
      if (pinned.current) params.set('sourceRevision', pinned.current);
      if (block.cursor) params.set('cursor', block.cursor);
      else if (index === 0 && route.match && !saved.current && !ignoreMatch.current) params.set('record', route.match);
      const page = await libraryRead<LibraryRecord>(`entries/${encodeURIComponent(route.entry)}`, params, controller.signal);
      if (controller.signal.aborted || disposed.current) return;
      if (!page.entry || page.entry.entryId !== route.entry || (pinned.current && page.entry.sourceRevision !== pinned.current)) throw new Error('source changed');
      pinned.current = page.entry.sourceRevision;
      cache.current.set(index, page);
      for (const key of cache.current.keys()) if (Math.abs(key - activeRef.current) > 2) cache.current.delete(key);
      setEntry(page.entry); setVersion(v => v + 1); setError('');
      if (page.nextCursor && index + 1 === blocksRef.current.length && blocksRef.current.length < 2048) {
        setBlocks(previous => [...previous, { cursor: page.nextCursor, height: 700 }]);
      }
      if (seeking.current && index === seekIndex.current) {
        const found = page.records.some(record => ['conversation', 'attention'].includes(recordPresentation(record)));
        if (found) { seeking.current = false; if (index > 0) pendingSeek.current = index; }
        else if (page.nextCursor && index < 7) { seekIndex.current = index + 1; setActive(index + 1); }
        else {
          seeking.current = false;
          setPrefixNotice(page.nextCursor ? '前 128 条记录尚未找到对话，可继续阅读后续记录或查看全部记录。' : '已读取的记录没有可展示的对话，上下文和过程仍可展开核查。');
        }
      }
    } catch (e) { if (!controller.signal.aborted && !disposed.current) setError(failure(e)); }
    finally { if (inflight.current === controller) inflight.current = null; if (!disposed.current && !controller.signal.aborted) setBusy(false); }
  };
  useEffect(() => {
    disposed.current = false;
    return () => { disposed.current = true; inflight.current?.abort(); };
  }, []);
  useEffect(() => {
    if (error || revoked) return;
    const index = Array.from({ length: end - start }, (_, i) => start + i).find(i => !cache.current.has(i));
    if (index !== undefined) void load(index);
  }, [active, blocks.length, version, retry, revoked]);
  useLayoutEffect(() => {
    const element = scroll.current;
    if (!element) return;
    const measure = () => {
      const heights = new Map<number, number>();
      element.querySelectorAll<HTMLElement>('[data-library-block]').forEach(node => heights.set(Number(node.dataset.libraryBlock), node.getBoundingClientRect().height || 700));
      const previous = blocksRef.current;
      const next = previous.map((block, i) => heights.has(i) && Math.abs(heights.get(i)! - block.height) > 1 ? { ...block, height: heights.get(i)! } : block);
      let above = 0, adjustment = 0;
      previous.forEach((block, i) => { above += block.height; if (above <= element.scrollTop) adjustment += next[i].height - block.height; });
      blocksRef.current = next;
      setBlocks(next);
      if (pendingSeek.current !== null && element.querySelector(`[data-library-block="${pendingSeek.current}"]`)) {
        element.scrollTop = next.slice(0, pendingSeek.current).reduce((n, b) => n + b.height, 0); pendingSeek.current = null;
      } else if (adjustment) element.scrollTop += adjustment;
    };
    measure();
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(measure) : null;
    element.querySelectorAll('[data-library-block]').forEach(node => observer?.observe(node));
    if (saved.current && cache.current.has(0)) { element.scrollTop = saved.current.offset; saved.current = undefined; }
    if (modeAnchor.current) {
      const anchor = modeAnchor.current;
      const node = Array.from(element.querySelectorAll<HTMLElement>('[data-record-cursor]')).find(node => node.dataset.recordCursor === anchor.cursor);
      if (node) element.scrollTop += node.getBoundingClientRect().top - anchor.top;
      modeAnchor.current = null;
    }
    return () => observer?.disconnect();
  }, [version, active, allRecords]);
  const restart = () => {
    inflight.current?.abort(); inflight.current = null; cache.current.clear(); ignoreMatch.current = true; pinned.current = ''; saved.current = undefined;
    setEntry(undefined); setBlocks([{ cursor: null, height: 700 }]); setActive(0); setError(''); setRetry(v => v + 1);
    seeking.current = true; seekIndex.current = 0; pendingSeek.current = null; setPrefixNotice('');
    if (scroll.current) scroll.current.scrollTop = 0;
    state.replace({ libraryReading: undefined });
  };
  useLayoutEffect(() => {
    if (detail === null && returnFocus.current) {
      returnFocus.current.focus({ preventScroll: true });
      returnFocus.current = null;
    }
  }, [detail]);
  const closeDetail = () => { setDetail(null); };
  const changeMode = (all: boolean) => {
    const element = scroll.current;
    if (element) {
      const top = element.getBoundingClientRect().top;
      const node = Array.from(element.querySelectorAll<HTMLElement>('[data-record-cursor]')).find(node => node.getBoundingClientRect().bottom > top);
      modeAnchor.current = node ? { cursor: node.dataset.recordCursor!, top: node.getBoundingClientRect().top } : null;
    }
    seeking.current = false; setAllRecords(all);
  };
  return <section className="wb-library-reader" aria-label="历史会话阅读">
    <header className="wb-library-read-heading"><button onClick={() => { remember(); onBack(); }}>← 返回会话列表</button><h2>{entry?.title || '读取历史会话'}</h2>{entry && <p>{sourceNames[entry.kind] || '历史记录'} · {dateLabel(entry.recordedAt)}</p>}{entry && onResume && <div className="wb-library-launch">{entry.kind === 'native' && entry.capabilities.resume && !entry.coverage.reasons.includes('inherited_history_not_loaded') ? <button className="wb-resume-button" disabled={revoked} onClick={() => onResume(entry)}>继续此会话</button> : <small className="wb-subtle">{entry.kind === 'native' ? '这条记录暂没有可核验的恢复入口。' : '此记录用于阅读；恢复需要对应的原生会话。'}</small>}</div>}</header>
    {entry && !revoked && (entry.isSubagent || entry.parentThreadId || entry.coverage.reasons.includes('conflicting_parent_identity')) && <nav className="wb-library-parent" aria-label="父会话导航">{entry.parentEntryId ? <button onClick={() => { remember(); onEntry(entry.parentEntryId!); }}>查看父会话</button> : <span>{entry.coverage.reasons.includes('conflicting_parent_identity') ? '父关系信息冲突，无法导航' : '父会话当前不可用'}</span>}</nav>}
    {entry && <details className="wb-library-identity"><summary>记录信息与覆盖范围</summary><p>项目：{entry.projectId ? entry.projectPath || '项目路径未记录' : '未归类会话'}</p>{!entry.projectId && <p>未归类原因：{unassignedReasonText(entry.unassignedReason)}</p>}<p>工作目录：{entry.recordedCwd ?? entry.projectPath ?? '未记录'}</p><p>来源：{entry.sourceId}</p><p>会话：{entry.nativeThreadId || entry.runId || '原生身份未记录'}</p>{entry.parentThreadId && <p>子代理 {entry.agentName || ''} · 父会话：{entry.parentThreadId}</p>}<p>{coverageText(first?.coverage || entry.coverage) || '显示此来源已保存的记录；不代表其它来源或整个任务的完整内容。'}</p>{entry.parentThreadId && !entry.parentEntryId && <p>父会话当前不可用</p>}{!revoked && entry.relatedEntryIds.map(id => <button key={id} onClick={() => onEntry(id)}>查看关联来源的记录</button>)}</details>}
    {organizationUnavailable && <p className="wb-library-note" role="status">用户归类暂不可用；原始记录仍可阅读，继续会话使用原工作目录。</p>}
    {entry && instanceId && !revoked && !organizationUnavailable && <div className="wb-library-organize"><button ref={organizationButton} onClick={() => { remember(); setEditing(true); }}>调整项目归属</button></div>}
    {editing && entry && instanceId && !revoked && !organizationUnavailable && <OrganizationEditor entry={entry} instanceId={instanceId} onClose={() => { setEditing(false); organizationButton.current?.focus(); }} onSaved={value => { setEntry(previous => previous ? { ...previous, projectId: value.organization.effective.projectId, projectPath: value.organization.effective.projectPath, projectBasis: value.organization.effective.basis, unassignedReason: value.organization.unassignedReason, organization: value.organization } : previous); onOrganizationSaved?.(value); }} />}
    {route.match && <p className="wb-library-note">从搜索命中的记录附近开始阅读，位置绑定当前来源版本。</p>}
    {revoked ? <p className="wb-notice" role="alert">此来源已被移除，已停止展示它的内容。请返回会话列表。</p> : <>
      {error && <div className="wb-notice" role="alert">{error}<button onClick={() => { setError(''); setRetry(v => v + 1); }}>重试当前位置</button><button onClick={restart}>重新读取会话</button></div>}
      <nav className="wb-library-reading-mode" aria-label="正文显示方式"><button aria-pressed={!allRecords} onClick={() => changeMode(false)}>对话</button><button aria-pressed={allRecords} onClick={() => changeMode(true)}>全部记录</button></nav>
      {prefixNotice && <p className="wb-library-note" role="status">{prefixNotice}</p>}
      <div className="wb-library-body-scroll" ref={scroll} tabIndex={0} aria-label="会话正文" onScroll={() => {
        // A click can scroll a visible record before its scroll event is delivered.
        // Keep the inspected window mounted until the detail returns focus to it.
        if (returnFocus.current) return;
        seeking.current = false;
        let i = remember();
        const area = scroll.current!;
        const viewport = area.getBoundingClientRect();
        const visible = Array.from(area.querySelectorAll<HTMLElement>('[data-library-block]')).filter(node => {
          const rect = node.getBoundingClientRect();
          return rect.bottom > viewport.top && rect.top < viewport.bottom;
        }).map(node => Number(node.dataset.libraryBlock));
        // Short folded windows can share the viewport. Keep every visible mounted
        // window inside the three-window range, including a button being clicked.
        if (visible.length) i = Math.max(Math.max(...visible) - 1, Math.min(i, Math.min(...visible) + 1));
        if (i !== active) setActive(i);
      }}>
        <div aria-hidden="true" style={{ height: blocks.slice(0, start).reduce((n, b) => n + b.height, 0) }} />
        {blocks.slice(start, end).map((block, local) => {
          const index = start + local, page = cache.current.get(index);
          return page ? <div key={index} data-library-block={index}>
            {allRecords && page.window != null && <p className="wb-library-note">保存窗口 · 观察位置 {page.window}</p>}
            <RecordWindow records={page.records} all={allRecords} reveal={!!route.match && index === 0 && !ignoreMatch.current} onInspect={(cursor, button) => { remember(); returnFocus.current = button; setActive(index); setDetail(cursor); }} />
            {!page.records.length && !page.nextCursor && <p className="wb-subtle">此窗口没有可展示的正文。</p>}
            {page.nextCursor && blocks.length >= 2048 && index === blocks.length - 1 && <button onClick={() => { cache.current.clear(); setBlocks([{ cursor: page.nextCursor, height: 700 }]); setActive(0); setRetry(n => n + 1); if (scroll.current) scroll.current.scrollTop = 0; }}>继续读取后续记录</button>}
            {!page.nextCursor && <p className="wb-library-end">已读到此来源的记录末尾</p>}
          </div> : <div key={index} style={{ minHeight: block.height }} className="wb-library-loading"><span role="status">{busy ? '正在读取后续记录…' : '后续记录将在滚动时加载'} </span>{!busy && <button onClick={() => { setActive(index); setError(''); setRetry(v => v + 1); }}>继续阅读</button>}</div>;
        })}
        <div aria-hidden="true" style={{ height: blocks.slice(end).reduce((n, b) => n + b.height, 0) }} />
      </div>
      {busy && <span className="wb-library-read-status" role="status">正在读取…</span>}
      {detail && <RecordDetail key={detail} id={route.entry} revision={pinned.current} cursor={detail} onClose={closeDetail} />}
    </>}
  </section>;
}
function RecordDetail({ id, revision, cursor, onClose }: { id: string; revision: string; cursor: string; onClose: () => void }) {
  const [text, setText] = useState(''); const [error, setError] = useState(''); const [length, setLength] = useState(16000); const close = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    close.current?.focus({ preventScroll: true }); const controller = new AbortController();
    void libraryRead<unknown>(`entries/${encodeURIComponent(id)}/details`, new URLSearchParams({ window: 'true', sourceRevision: revision, cursor }), controller.signal).then(page => {
      if (!controller.signal.aborted) setText(JSON.stringify(page.records.map(record => {
        if (record && typeof record === 'object') { const { detailCursor: _cursor, ...fields } = record as Record<string, unknown>; return fields; } return record;
      }), null, 2));
    }).catch(e => { if (!controller.signal.aborted) setError(failure(e)); });
    return () => controller.abort();
  }, [id, revision, cursor]);
  return <section className="wb-library-detail" aria-label="记录详情" onKeyDown={e => { if (e.key === 'Escape') { e.stopPropagation(); onClose(); } }}>
    <header><strong>记录详情</strong><button ref={close} onClick={onClose}><Icon name="close" /> 关闭详情</button></header>
    <div><p className="wb-subtle">这是当前来源保存的脱敏字段。未记录的提示词、用量或执行结果不会补造。</p>{error && <p role="alert" className="wb-notice">{error}</p>}{text ? <><pre tabIndex={0}>{text.slice(0, length)}</pre>{length < text.length && <button onClick={() => setLength(n => n + 16000)}>继续展开详情（剩余 {(text.length - length).toLocaleString()} 字符）</button>}</> : !error && <p role="status">正在读取详情…</p>}</div>
  </section>;
}
