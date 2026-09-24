import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { HistoryRecord } from './HistoryRecord';
import { Icon } from './Icons';
import { coverageText, dateLabel, failure, libraryRead, sourceNames, type HistoryRoute, type LibraryEntry, type LibraryPage, type LibraryRecord } from './library';
interface Block { cursor: string | null; height: number }
interface SavedPosition { entry: string; revision: string; cursor: string | null; offset: number }
export function LibraryReader({ route, onBack, onEntry, sources, onResume }: { onResume?: (entry: LibraryEntry) => void; route: HistoryRoute; onBack: () => void; onEntry: (id: string) => void; sources: { id: string; identity?: string }[] }) {
  const saved = useRef<SavedPosition | undefined>(history.state?.libraryReading?.entry === route.entry ? history.state.libraryReading : undefined);
  const [blocks, setBlocks] = useState<Block[]>([{ cursor: saved.current?.cursor || null, height: 700 }]);
  const blocksRef = useRef(blocks); blocksRef.current = blocks;
  const cache = useRef(new Map<number, LibraryPage<LibraryRecord>>());
  const [version, setVersion] = useState(0);
  const [active, setActive] = useState(0);
  const [entry, setEntry] = useState<LibraryEntry>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [detail, setDetail] = useState<string | null>(null);
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
  const remember = () => {
    const top = scroll.current?.scrollTop || 0;
    let i = 0, height = 0;
    while (i + 1 < blocksRef.current.length && height + blocksRef.current[i].height < top) height += blocksRef.current[i++].height;
    history.replaceState({ ...history.state, libraryReading: { entry: route.entry, revision: pinned.current, cursor: blocksRef.current[i].cursor, offset: Math.max(0, top - height) } }, '');
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
      for (const key of cache.current.keys()) if (Math.abs(key - active) > 2) cache.current.delete(key);
      setEntry(page.entry); setVersion(v => v + 1); setError('');
      if (page.nextCursor && index + 1 === blocksRef.current.length && blocksRef.current.length < 2048) {
        setBlocks(previous => [...previous, { cursor: page.nextCursor, height: 700 }]);
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
      setBlocks(previous => previous.map((block, i) => heights.has(i) && Math.abs(heights.get(i)! - block.height) > 1 ? { ...block, height: heights.get(i)! } : block));
    };
    measure();
    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(measure) : null;
    element.querySelectorAll('[data-library-block]').forEach(node => observer?.observe(node));
    if (saved.current && cache.current.has(0)) { element.scrollTop = saved.current.offset; saved.current = undefined; }
    return () => observer?.disconnect();
  }, [version, active]);
  const restart = () => {
    inflight.current?.abort(); inflight.current = null; cache.current.clear(); ignoreMatch.current = true; pinned.current = ''; saved.current = undefined;
    setEntry(undefined); setBlocks([{ cursor: null, height: 700 }]); setActive(0); setError(''); setRetry(v => v + 1);
    if (scroll.current) scroll.current.scrollTop = 0;
    history.replaceState({ ...history.state, libraryReading: undefined }, '');
  };
  const closeDetail = () => { setDetail(null); returnFocus.current?.focus(); };
  return <section className="wb-library-reader" aria-label="历史会话阅读">
    <header className="wb-library-read-heading"><button onClick={() => { remember(); onBack(); }}>← 返回会话列表</button><h2>{entry?.title || '读取历史会话'}</h2>{entry && <p>{sourceNames[entry.kind] || '历史记录'} · {dateLabel(entry.recordedAt)}</p>}{entry && onResume && <div className="wb-library-launch">{entry.kind === 'native' && entry.capabilities.resume && !entry.coverage.reasons.includes('inherited_history_not_loaded') ? <button disabled={revoked} onClick={() => onResume(entry)}>继续此会话</button> : <small className="wb-subtle">{entry.kind === 'native' ? '这条记录暂没有可核验的恢复入口。' : '此记录用于阅读；恢复需要对应的原生会话。'}</small>}</div>}</header>
    {entry && <details className="wb-library-identity"><summary>记录信息与覆盖范围</summary><p>项目：{entry.projectId ? entry.projectPath || '项目路径未记录' : '未归属项目'}</p><p>工作目录：{entry.recordedCwd ?? entry.projectPath ?? '未记录'}</p><p>来源：{entry.sourceId}</p><p>会话：{entry.nativeThreadId || entry.runId || '原生身份未记录'}</p>{entry.parentThreadId && <p>子代理 {entry.agentName || ''} · 父会话：{entry.parentThreadId}</p>}<p>{coverageText(first?.coverage || entry.coverage) || '显示此来源已保存的记录；不代表其它来源或整个任务的完整内容。'}</p>{entry.parentThreadId && !entry.parentEntryId && <p>父会话当前不可用</p>}{entry.parentEntryId && <button disabled={revoked} onClick={() => onEntry(entry.parentEntryId!)}>查看父会话</button>}{entry.relatedEntryIds.map(id => <button key={id} disabled={revoked} onClick={() => onEntry(id)}>查看关联来源的记录</button>)}</details>}
    {route.match && <p className="wb-library-note">从搜索命中的记录附近开始阅读，位置绑定当前来源版本。</p>}
    {revoked ? <p className="wb-notice" role="alert">此来源已被移除，已停止展示它的内容。请返回会话列表。</p> : <>
      {error && <div className="wb-notice" role="alert">{error}<button onClick={() => { setError(''); setRetry(v => v + 1); }}>重试当前位置</button><button onClick={restart}>重新读取会话</button></div>}
      <div className="wb-library-body-scroll" ref={scroll} tabIndex={0} aria-label="会话正文" onScroll={() => {
        const i = remember(); if (i !== active) setActive(i);
      }}>
        <div aria-hidden="true" style={{ height: blocks.slice(0, start).reduce((n, b) => n + b.height, 0) }} />
        {blocks.slice(start, end).map((block, local) => {
          const index = start + local, page = cache.current.get(index);
          return page ? <div key={index} data-library-block={index}>
            {page.window != null && <p className="wb-library-note">较早的保存窗口 · 观察位置 {page.window}</p>}
            {page.records.map((record, i) => <HistoryRecord key={`${index}:${i}`} record={record} onInspect={(cursor, button) => { remember(); returnFocus.current = button; setDetail(cursor); }} />)}
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
    close.current?.focus(); const controller = new AbortController();
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
