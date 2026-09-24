import { useEffect, useRef, useState } from 'preact/hooks';
import { LibraryReader } from './LibraryReader';
import { Icon } from './Icons';
import { coverageText, dateLabel, failure, libraryRead, projectName, routeFromURL, routeURL, sourceNames, type HistoryRoute, type LibraryEntry, type LibraryPage, type Project } from './library';
export interface LibrarySource { id: string; kind: string; state: string; message: string; identity?: string; revision?: string | null; indexedEntries?: number }
interface Paging { filters: string; refresh: number; cursors: (string | null)[]; index: number; base: number }
function usePage<T>(path: string, filters: string, refresh: number) {
  const restore = (): Paging => {
    const saved = history.state?.libraryLists?.[path];
    const valid = saved?.filters === filters && Array.isArray(saved.cursors) && saved.cursors.length <= 512 && saved.cursors.every((c: unknown) => c === null || typeof c === 'string') && Number.isInteger(saved.index) && saved.index >= 0 && saved.index < saved.cursors.length;
    return { filters, refresh, cursors: valid ? saved.cursors : [null], index: valid ? saved.index : 0, base: valid ? saved.base || 0 : 0 };
  };
  const [paging, setPaging] = useState<Paging>(restore);
  const selected = paging.filters === filters && paging.refresh === refresh ? paging : paging.refresh !== refresh ? { filters, refresh, cursors: [null], index: 0, base: 0 } : restore();
  const cursor = selected.cursors[selected.index];
  const identity = `${filters}:${refresh}:${cursor || ''}`;
  const [result, setResult] = useState<{ identity: string; page: LibraryPage<T> }>();
  const page = result?.identity === identity ? result.page : undefined;
  const [error, setError] = useState(''); const [busy, setBusy] = useState(false); const [retry, setRetry] = useState(0);
  const request = useRef(0);
  useEffect(() => { if (selected !== paging) setPaging(selected); }, [filters, refresh]);
  useEffect(() => {
    if (!page || busy || error) return;
    history.replaceState({ ...history.state, libraryLists: { ...history.state?.libraryLists, [path]: selected } }, '');
  }, [page, busy, error]);
  useEffect(() => {
    const controller = new AbortController(), sequence = ++request.current;
    setBusy(true); setError('');
    const params = new URLSearchParams(filters); params.set('limit', '30'); if (cursor) params.set('cursor', cursor);
    void libraryRead<T>(path, params, controller.signal).then(value => { if (!controller.signal.aborted && sequence === request.current) setResult({ identity, page: value }); }).catch(e => { if (!controller.signal.aborted && sequence === request.current) setError(failure(e)); }).finally(() => { if (!controller.signal.aborted && sequence === request.current) setBusy(false); });
    return () => controller.abort();
  }, [path, identity, retry]);
  return { page, error, busy, index: selected.base + selected.index, canPrevious: selected.index > 0,
    reload: () => setRetry(n => n + 1),
    reset: () => { setPaging({ filters, refresh, cursors: [null], index: 0, base: 0 }); setRetry(n => n + 1); },
    previous: () => setPaging({ ...selected, index: Math.max(0, selected.index - 1) }),
    next: () => { if (page?.nextCursor) { const cursors = [...selected.cursors.slice(0, selected.index + 1), page.nextCursor]; const drop = Math.max(0, cursors.length - 512); setPaging({ ...selected, cursors: cursors.slice(drop), index: cursors.length - 1 - drop, base: selected.base + drop }); } }
  };
}
export function HistoryLibraryPanel({ sources, onProjectLaunch, onResume, runs = [] }: { sources: LibrarySource[]; onProjectLaunch?: (projectId: string) => void; onResume?: (entry: LibraryEntry) => void; runs?: { runId: string; projectPath: string; state: string }[] }) {
  const [route, setRoute] = useState<HistoryRoute>(routeFromURL);
  const [draft, setDraft] = useState(route.q);
  const [refresh, setRefresh] = useState(0);
  const [notice, setNotice] = useState('');
  useEffect(() => {
    if (!notice.startsWith('已安排后台更新')) return;
    const timer = window.setTimeout(() => setNotice(current => current.startsWith('已安排后台更新') ? '' : current), 3000);
    return () => clearTimeout(timer);
  }, [notice]);
  const [mobileProjects, setMobileProjects] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const backFocus = useRef<string>('');
  const nav = (values: Partial<HistoryRoute>) => {
    if (!route.entry) history.replaceState({ ...history.state, libraryListScroll: list.current?.scrollTop || 0 }, '');
    const next = { ...route, ...values };
    history.pushState({ libraryListScroll: history.state?.libraryListScroll || 0, libraryLists: history.state?.libraryLists }, '', routeURL(next));
    setRoute(next); setDraft(next.q);
  };
  useEffect(() => {
    const change = () => { const next = routeFromURL(); setRoute(next); setDraft(next.q); };
    addEventListener('popstate', change); return () => removeEventListener('popstate', change);
  }, []);
  const params = new URLSearchParams();
  if (route.source) params.set('sourceId', route.source);
  if (route.group) params.set('group', route.group);
  if (route.q) params.set('q', route.q);
  const projects = usePage<Project>('projects', params.toString(), refresh);
  if (route.project) params.set('projectId', route.project);
  const entries = usePage<LibraryEntry>('entries', params.toString(), refresh);
  useEffect(() => {
    if (!route.project || route.project === 'unassigned' || entries.page?.projectExists !== false) return;
    const next = { ...route, project: '' };
    history.replaceState(history.state, '', routeURL(next));
    setRoute(next);
    setNotice('项目归类已更新，已清除原项目筛选。会话记录仍可在全部会话或未归属项目中查看。');
  }, [route.project, entries.page]);
  const revisions = useRef(new Map<string, string>());
  for (const source of sources) if (source.revision) revisions.current.set(source.id, source.revision);
  const sourceSignature = sources.map(s => `${s.id}:${revisions.current.get(s.id) || ''}`).join('|');
  const sourceIds = sources.map(s => `${s.id}:${s.identity || ""}`).join('|');
  const previousIds = useRef(sourceIds);
  useEffect(() => { if (previousIds.current !== sourceIds) { setRefresh(n => n + 1); previousIds.current = sourceIds; } }, [sourceIds]);
  const visibleEntries = entries.page?.records.filter(e => sources.some(s => s.id === e.sourceId && (!e.sourceIdentity || !s.identity || e.sourceIdentity === s.identity))) || [];
  // A published generation may arrive between status polls, including in a
  // read-only instance. Retry only incomplete empty queries, without rescanning
  // sources or resetting the user's filters, pagination, or reader.
  const pendingCatalog = !!entries.page && (entries.page.revision === 'pending' || entries.page.coverage.reasons.some(reason => reason === 'indexing' || reason.startsWith('source_indexing:')));
  useEffect(() => {
    if (route.entry || entries.busy || entries.error || visibleEntries.length || !pendingCatalog) return;
    const timer = window.setTimeout(() => { entries.reload(); projects.reload(); }, 2000);
    return () => clearTimeout(timer);
  }, [route.entry, entries.page, entries.busy, entries.error, pendingCatalog, visibleEntries.length]);
  const indexedEntries = sources.filter(source => !route.source || source.id === route.source).reduce((sum, source) => sum + (source.indexedEntries || 0), 0);
  const previousSignature = useRef(sourceSignature);
  useEffect(() => {
    if (previousSignature.current !== sourceSignature) {
      if (!entries.page?.records.length && !route.entry) setRefresh(n => n + 1);
      else setNotice('历史目录有更新，刷新列表可查看最新记录。当前阅读位置保持不变。');
      previousSignature.current = sourceSignature;
    }
  }, [sourceSignature]);
  useEffect(() => {
    if (!route.entry && list.current) {
      list.current.scrollTop = history.state?.libraryListScroll || 0;
      if (backFocus.current) Array.from(list.current.querySelectorAll<HTMLButtonElement>("[data-entry]")).find(b => b.dataset.entry === backFocus.current)?.focus({ preventScroll: true });
    }
  }, [route.entry, entries.page]);
  const filter = (values: Partial<HistoryRoute>) => { nav({ ...values, entry: '', revision: '', match: '' }); setMobileProjects(false); };
  const open = (entry: LibraryEntry) => { backFocus.current = entry.entryId; nav({ entry: entry.entryId, revision: entry.sourceRevision, match: entry.match ? String(entry.match.record) : '' }); };
  const update = async () => {
    setNotice(''); setRefresh(n => n + 1);
    try {
      const response = await fetch('/workbench/v1/library/refresh', { method: 'POST', credentials: 'same-origin' });
      if (!response.ok) throw new Error();
      setNotice('已安排后台更新，已有记录可以继续阅读。');
    } catch { setNotice('连接暂时不可用，未能更新目录。'); }
  };
  const projectPath = projects.page?.records.find(p => p.projectId === route.project)?.path || entries.page?.records[0]?.projectPath;
  const activeRun = runs.find(run => run.projectPath === projectPath && run.state === 'running');
  const pageControls = (value: typeof projects | typeof entries, name: string) => <div className="wb-library-pagination" aria-label={name}><button disabled={value.busy || !value.canPrevious} onClick={value.previous}>上一页</button><span>第 {value.index + 1} 页</span><button disabled={value.busy || !value.page?.nextCursor} onClick={value.next}>下一页</button></div>;
  return <main className={`wb-library ${route.entry ? 'is-reading' : ''}`}>
    <aside className={`wb-library-projects ${mobileProjects ? 'is-open' : ''}`} aria-label="项目导航">
      <div className="wb-library-sidebar-heading"><Icon name="folderOpen" /><strong>项目</strong><button className="wb-library-project-close" onClick={() => setMobileProjects(false)}>收起</button></div>
      <button className={`wb-library-project ${!route.project ? 'selected' : ''}`} aria-pressed={!route.project} onClick={() => filter({ project: '' })}><Icon name="history" /><span>全部会话</span></button>
      {((projects.page?.unassignedRecords || 0) > 0 || route.project === 'unassigned') && <button className={`wb-library-project ${route.project === 'unassigned' ? 'selected' : ''}`} aria-pressed={route.project === 'unassigned'} onClick={() => filter({ project: 'unassigned' })}><Icon name="chat" /><span>未归属项目</span>{projects.page?.unassignedRecords !== undefined && <em>{projects.page.unassignedRecords} 条记录</em>}</button>}
      <p className="wb-library-project-order">项目 · 最近记录优先</p>
      <div className="wb-library-project-list">
        {projects.page?.records.filter(project => project.projectId).map(project => <button key={project.projectId || 'unassigned'} className={`wb-library-project ${route.project === (project.projectId || 'unassigned') ? 'selected' : ''}`} aria-pressed={route.project === (project.projectId || 'unassigned')} onClick={() => filter({ project: project.projectId || 'unassigned' })} title={project.path || '没有可靠路径的历史记录'}><Icon name={project.path ? 'folder' : 'unavailable'} /><span><strong>{projectName(project.path)}</strong>{project.path && <small>{project.path}</small>}</span><em>{project.entries} 条记录</em></button>)}
        {projects.busy && <p role="status" className="wb-subtle">正在读取项目…</p>}{projects.error && <p role="alert" className="wb-notice">{projects.error}<button onClick={projects.reset}>重新读取</button></p>}
      </div>
      {(projects.index > 0 || projects.page?.nextCursor) && pageControls(projects, '项目分页')}
      <details className="wb-library-sources"><summary>历史来源 · {sources.length}</summary><ul className="wb-home-sources">{sources.map(source => <li key={source.id}><strong>{sourceNames[source.kind] || '历史来源'}<small>{source.id}</small></strong><span role={source.state === 'unavailable' ? 'status' : undefined}>{source.message}</span></li>)}</ul></details>
    </aside>
    <section className="wb-library-main">
      <div className="wb-library-toolbar"><button className="wb-library-project-toggle" onClick={() => setMobileProjects(v => !v)} aria-expanded={mobileProjects}><Icon name="folder" /> 项目</button><h1>全部历史</h1><button onClick={() => void update()} title="重新检查已登记的来源"><Icon name="refresh" /> 更新历史</button></div>
      <div className="wb-library-filters"><form onSubmit={e => { e.preventDefault(); filter({ q: draft.trim() }); }}><Icon name="search" /><input type="search" aria-label="搜索会话和消息" placeholder="搜索会话和消息" maxLength={512} value={draft} onInput={e => setDraft(e.currentTarget.value)} /><button type="submit">搜索</button></form><select aria-label="历史来源筛选" value={route.source} onChange={e => filter({ source: e.currentTarget.value })}><option value="">全部来源</option>{sources.map(s => <option key={s.id} value={s.id}>{sourceNames[s.kind] || '历史'}{sources.filter(other => other.kind === s.kind).length > 1 ? ` · 来源 ${sources.filter(other => other.kind === s.kind).findIndex(other => other.id === s.id) + 1}` : ''}</option>)}</select><select aria-label="会话分组" value={route.group} onChange={e => filter({ group: e.currentTarget.value })}><option value="">全部会话</option><option value="main">主会话</option><option value="agents">子代理</option></select></div>
      {notice && <div className="wb-library-note" role="status">{notice}<button onClick={() => { setRefresh(n => n + 1); setNotice(''); }}>刷新列表</button></div>}
      <div className="wb-library-entry-list" ref={list} hidden={!!route.entry}>
        <div className="wb-library-list-heading"><strong>{route.project ? route.project === 'unassigned' ? '未归属项目' : projectPath ? projectName(projectPath) : '所选项目' : '最近的会话'}</strong>{route.project && route.project !== 'unassigned' && onProjectLaunch && (activeRun ? <a href={`/?run=${encodeURIComponent(activeRun.runId)}`}>进入工作台</a> : <button onClick={() => onProjectLaunch(route.project)}>开始新对话</button>)}{route.q && <button onClick={() => filter({ q: '' })}>清除“{route.q}”</button>}</div>
        {entries.page && coverageText(entries.page.coverage) && <p className="wb-library-note">{coverageText(entries.page.coverage)}</p>}
        {entries.error && <p role="alert" className="wb-notice">{entries.error}<button onClick={entries.reset}>重新读取列表</button></p>}
        {entries.busy && <p role="status" className="wb-subtle">正在读取历史…</p>}
        {visibleEntries.map(entry => <button data-entry={entry.entryId} key={entry.entryId} className="wb-library-entry" onClick={() => open(entry)}>
          <span className={`wb-library-entry-icon is-${entry.kind}`}><Icon name={entry.isSubagent || entry.parentThreadId ? 'git' : 'chat'} /></span><span className="wb-library-entry-text"><strong>{entry.title}</strong><span>{entry.projectId ? projectName(entry.projectPath) : '未归属项目'} <span className="wb-library-source-tag">{sourceNames[entry.kind] || '历史记录'}</span>{(entry.isSubagent || entry.parentThreadId) && <span className="wb-library-source-tag">子代理 · {entry.agentName || entry.parentThreadId?.slice(0, 8) || '未命名'}</span>}</span>{entry.match && <small className="wb-library-hit">{entry.match.text}</small>}<small className="wb-library-path">{entry.projectId ? entry.projectPath : '工作目录可在记录信息中查看'}</small></span><time>{dateLabel(entry.recordedAt)}</time><Icon name="chevron" />
        </button>)}
        {!entries.busy && !entries.error && !visibleEntries.length && <div className="wb-library-empty"><Icon name="history" /><h2>{pendingCatalog ? '正在整理历史' : sources.every(s => s.state === 'unavailable') ? '历史来源暂不可用' : route.q || route.project || route.source || route.group ? '没有匹配的记录' : '这里还没有历史记录'}</h2><p>{pendingCatalog ? indexedEntries > 0 ? `已整理 ${indexedEntries} 条记录，目录仍在更新，完成的记录会自动显示。` : '目录仍在更新，完成的记录会自动显示。' : sources.some(s => s.state === 'unavailable') ? '部分历史位置无法读取。展开“历史来源”或设置检查已登记的位置。' : '可在设置中接入其它历史来源，或调整当前筛选。'}</p></div>}
        {(entries.index > 0 || entries.page?.nextCursor) && pageControls(entries, '会话分页')}
      </div>
      {route.entry && <LibraryReader key={`${route.entry}:${route.revision}:${route.match}`} route={route} sources={sources} onResume={onResume} onBack={() => nav({ entry: '', revision: '', match: '' })} onEntry={id => nav({ entry: id, revision: '', match: '' })} />}
    </section>
  </main>;
}
