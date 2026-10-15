import { useEffect, useRef, useState } from 'preact/hooks';
import { LibraryReader } from './LibraryReader';
import { SessionGroupList } from './SessionGroupList';
import { Icon } from './Icons';
import { ExchangePreview } from './ExchangePreview';
import { coverageText, dateLabel, failure, historyView, libraryRead, projectName, routeFromURL, routeURL, sourceNames, type GroupMember, type HistoryRoute, type LibraryEntry, type LibraryPage, type Project, type SessionGroup, type SessionSection } from './library';
export interface LibrarySource { id: string; kind: string; state: string; message: string; identity?: string; revision?: string | null; indexedEntries?: number }
interface Paging { filters: string; refresh: number; cursors: (string | null)[]; index: number; base: number }
type Notice = { kind: 'scanning' | 'available' | 'failed' | 'info'; text: string };
function usePage<T>(path: string, filters: string, refresh: number, enabled = true, stateKey = path) {
  const restore = (): Paging => {
    const saved = history.state?.libraryLists?.[stateKey];
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
    if (!enabled || !page || busy || error) return;
    history.replaceState({ ...history.state, libraryLists: { ...history.state?.libraryLists, [stateKey]: selected } }, '');
  }, [page, busy, error]);
  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController(), sequence = ++request.current;
    setBusy(true); setError('');
    const params = new URLSearchParams(filters); params.set('limit', '30'); if (cursor) params.set('cursor', cursor);
    void libraryRead<T>(path, params, controller.signal).then(value => { if (!controller.signal.aborted && sequence === request.current) setResult({ identity, page: value }); }).catch(e => { if (!controller.signal.aborted && sequence === request.current) setError(failure(e)); }).finally(() => { if (!controller.signal.aborted && sequence === request.current) setBusy(false); });
    return () => controller.abort();
  }, [path, identity, retry, enabled]);
  return { page, error, busy, index: selected.base + selected.index, canPrevious: selected.index > 0,
    reload: () => setRetry(n => n + 1),
    reset: () => { setPaging({ filters, refresh, cursors: [null], index: 0, base: 0 }); setRetry(n => n + 1); },
    previous: () => setPaging({ ...selected, index: Math.max(0, selected.index - 1) }),
    next: () => { if (page?.nextCursor) { const cursors = [...selected.cursors.slice(0, selected.index + 1), page.nextCursor]; const drop = Math.max(0, cursors.length - 512); setPaging({ ...selected, cursors: cursors.slice(drop), index: cursors.length - 1 - drop, base: selected.base + drop }); } }
  };
}
export function HistoryLibraryPanel({ sources, instanceId, onProjectLaunch, onResume, runs = [] }: { instanceId?: string; sources: LibrarySource[]; onProjectLaunch?: (projectId: string) => void; onResume?: (entry: LibraryEntry) => void; runs?: { runId: string; projectPath: string; state: string }[] }) {
  const [route, setRoute] = useState<HistoryRoute>(routeFromURL);
  const [draft, setDraft] = useState(route.q);
  const [projectDraft, setProjectDraft] = useState(route.projectQ || '');
  const [refresh, setRefresh] = useState(0);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [refreshId, setRefreshId] = useState<number | null>(null);
  const refreshRequest = useRef<AbortController | null>(null);
  useEffect(() => () => refreshRequest.current?.abort(), []);
  const [mobileProjects, setMobileProjects] = useState(false);
  const list = useRef<HTMLDivElement>(null);
  const backFocus = useRef<string>('');
  const organizationReading = useRef<string>('');
  const nav = (values: Partial<HistoryRoute>) => {
    if (!route.entry) history.replaceState({ ...history.state, libraryListScroll: list.current?.scrollTop || 0 }, '');
    const next = { ...route, ...values };
    history.pushState({ libraryListScroll: history.state?.libraryListScroll || 0, libraryLists: history.state?.libraryLists }, '', routeURL(next));
    setRoute(next); setDraft(next.q); setProjectDraft(next.projectQ || '');
  };
  useEffect(() => {
    const change = () => { const next = routeFromURL(); setRoute(next); setDraft(next.q); setProjectDraft(next.projectQ || ''); };
    addEventListener('popstate', change); return () => removeEventListener('popstate', change);
  }, []);
  const params = new URLSearchParams();
  const view = historyView(route);
  const section: SessionSection = route.section === 'auxiliary' || route.section === 'runs' ? route.section : 'primary';
  if (route.source) params.set('sourceId', route.source);
  if (view === 'records' && route.group) params.set('group', route.group);
  if (route.q) params.set('q', route.q);
  const projectParams = new URLSearchParams();
  if (route.projectQ) projectParams.set('projectQ', route.projectQ);
  const projects = usePage<Project>('projects', projectParams.toString(), refresh);
  if (route.project) params.set('projectId', route.project);
  if (view === 'groups') params.set('section', 'primary');
  const entries = usePage<LibraryEntry | SessionGroup>(view === 'records' ? 'entries' : 'session-groups', params.toString(), refresh, true, view === 'records' ? 'entries' : 'session-groups-primary');
  const extraParams = new URLSearchParams(params); extraParams.set('section', section);
  const extra = usePage<SessionGroup>('session-groups', extraParams.toString(), refresh, view === 'groups' && section !== 'primary', `session-groups-${section}`);
  const extraMismatch = view === 'groups' && section !== 'primary' && !!entries.page && !!extra.page && entries.page.revision !== extra.page.revision;
  useEffect(() => { if (extraMismatch) setNotice({ kind: 'available', text: '目录版本已变化，载入新结果后查看辅助会话或运行记录。当前阅读位置保持不变。' }); }, [extraMismatch]);
  useEffect(() => {
    if (!route.project || route.project === 'unassigned' || entries.page?.projectExists !== false || (route.entry && organizationReading.current === route.entry)) return;
    const next = { ...route, project: '' };
    history.replaceState(history.state, '', routeURL(next));
    setRoute(next);
    setNotice({ kind: 'info', text: '项目归类已更新，已清除原项目筛选。会话记录仍可在全部项目或未归类会话中查看。' });
  }, [route.project, route.entry, entries.page]);
  const revisions = useRef(new Map<string, string>());
  for (const source of sources) if (source.revision) revisions.current.set(source.id, source.revision);
  const sourceSignature = sources.map(s => `${s.id}:${revisions.current.get(s.id) || ''}`).join('|');
  const sourceIds = sources.map(s => `${s.id}:${s.identity || ""}`).join('|');
  const previousIds = useRef(sourceIds);
  useEffect(() => { if (previousIds.current !== sourceIds) { setRefresh(n => n + 1); previousIds.current = sourceIds; } }, [sourceIds]);
  const visibleEntries = view === 'records' ? (entries.page?.records as LibraryEntry[] | undefined)?.filter(e => sources.some(s => s.id === e.sourceId && (!e.sourceIdentity || !s.identity || e.sourceIdentity === s.identity))) || [] : [];
  const visibleGroups = view === 'groups' ? (entries.page?.records as SessionGroup[] | undefined)?.filter(group => group.members.some(member => member.entryId === group.preferredEntryId && sources.some(source => source.id === member.sourceId && (!member.sourceIdentity || !source.identity || member.sourceIdentity === source.identity)))) || [] : [];
  const visibleCount = view === 'groups' ? visibleGroups.length : visibleEntries.length;
  // A published generation may arrive between status polls, including in a
  // read-only instance. Retry only incomplete empty queries, without rescanning
  // sources or resetting the user's filters, pagination, or reader.
  const pendingCatalog = !!entries.page && (entries.page.revision === 'pending' || entries.page.coverage.reasons.some(reason => reason === 'indexing' || reason.startsWith('source_indexing:')));
  useEffect(() => {
    if (route.entry || entries.busy || entries.error || visibleCount || !pendingCatalog) return;
    const timer = window.setTimeout(() => { entries.reload(); projects.reload(); }, 2000);
    return () => clearTimeout(timer);
  }, [route.entry, entries.page, entries.busy, entries.error, pendingCatalog, visibleCount]);
  const indexedEntries = sources.filter(source => !route.source || source.id === route.source).reduce((sum, source) => sum + (source.indexedEntries || 0), 0);
  const previousSignature = useRef(sourceSignature);
  useEffect(() => {
    if (previousSignature.current !== sourceSignature) {
      if (!entries.page?.records.length && !route.entry) setRefresh(n => n + 1);
      setNotice({ kind: 'available', text: '新目录可读取，载入新结果可查看最新记录。当前阅读位置保持不变。' });
      previousSignature.current = sourceSignature;
    }
  }, [sourceSignature]);
  useEffect(() => {
    if (notice?.kind !== 'scanning' || refreshId === null) return;
    let disposed = false, timer: ReturnType<typeof setTimeout>;
    const controller = new AbortController();
    const poll = async () => {
      const timeout = setTimeout(() => controller.abort(), 5000);
      try {
        const response = await fetch('/workbench/v1/library/sources', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
        if (!response.ok) throw new Error('sources unavailable');
        const raw = await response.text();
        if (raw.length > 1024 * 1024) throw new Error('sources response budget');
        const status = JSON.parse(raw).refresh;
        if (!Number.isSafeInteger(status?.requested) || !Number.isSafeInteger(status?.completed) || typeof status?.failed !== 'boolean') throw new Error('invalid refresh status');
        if (disposed) return;
        if (status.completed >= refreshId) {
          setNotice(current => current?.kind !== 'scanning' ? current : status.failed
            ? { kind: 'failed', text: '扫描失败，部分来源暂不可用。已有记录保留。' }
            : { kind: 'info', text: '后台扫描已结束，当前阅读位置保持不变。新目录可读取时会提示载入。' });
        } else timer = setTimeout(() => void poll(), 1000);
      } catch {
        if (!disposed) setNotice(current => current?.kind === 'scanning' ? { kind: 'failed', text: '扫描失败，无法确认后台扫描结果。已有记录保留。' } : current);
      } finally { clearTimeout(timeout); }
    };
    timer = setTimeout(() => void poll(), 1000);
    return () => { disposed = true; clearTimeout(timer); controller.abort(); };
  }, [refreshId, notice?.kind]);
  useEffect(() => {
    if (!route.entry && list.current) {
      list.current.scrollTop = history.state?.libraryListScroll || 0;
      if (backFocus.current) Array.from(list.current.querySelectorAll<HTMLButtonElement>("[data-entry]")).find(b => b.dataset.entry === backFocus.current)?.focus({ preventScroll: true });
    }
  }, [route.entry, entries.page]);
  const filter = (values: Partial<HistoryRoute>) => { nav({ ...values, entry: '', revision: '', match: '' }); setMobileProjects(false); };
  const open = (entry: GroupMember) => { backFocus.current = entry.entryId; nav({ entry: entry.entryId, revision: entry.sourceRevision, match: entry.match ? String(entry.match.record) : '' }); };
  const update = async () => {
    refreshRequest.current?.abort();
    const controller = new AbortController(); refreshRequest.current = controller;
    setRefreshId(null);
    setNotice({ kind: 'scanning', text: '已安排后台扫描，结果发布前已有记录可以继续阅读。' });
    try {
      const response = await fetch('/workbench/v1/library/refresh', { method: 'POST', credentials: 'same-origin', signal: controller.signal });
      if (!response.ok) throw new Error();
      const result = await response.json();
      if (result.scheduled !== true || !Number.isSafeInteger(result.refreshId) || result.refreshId < 1) throw new Error();
      if (!controller.signal.aborted) setRefreshId(result.refreshId);
    } catch { if (!controller.signal.aborted) setNotice({ kind: 'failed', text: '扫描失败，未能安排后台更新。已有记录保留。' }); }
  };
  const selectedProject = entries.page?.selectedProject?.projectId === route.project ? entries.page.selectedProject : undefined;
  const projectPath = selectedProject?.path ?? projects.page?.records.find(p => p.projectId === route.project)?.path;
  const pageTitle = !route.project ? '全部历史' : route.project === 'unassigned' ? '未归类会话' : projectPath ? projectName(projectPath) : '所选项目';
  const showEntryProject = !route.project || route.project === 'unassigned';
  useEffect(() => { document.title = `${pageTitle} · Codex 工作台`; }, [pageTitle]);
  const activeRun = runs.find(run => run.projectPath === projectPath && run.state === 'running');
  const pageControls = (value: { busy: boolean; canPrevious: boolean; previous: () => void; next: () => void; index: number; page?: { nextCursor: string | null } }, name: string) => <div className="wb-library-pagination" aria-label={name}><button disabled={value.busy || !value.canPrevious} onClick={value.previous}>上一页</button><span>第 {value.index + 1} 页</span><button disabled={value.busy || !value.page?.nextCursor} onClick={value.next}>下一页</button></div>;
  return <main className={`wb-library ${route.entry ? 'is-reading' : ''}`}>
    <aside className={`wb-library-projects ${mobileProjects ? 'is-open' : ''}`} aria-label="项目导航">
      <div className="wb-library-sidebar-heading"><Icon name="folderOpen" /><strong>项目</strong><button className="wb-library-project-close" onClick={() => setMobileProjects(false)}>收起</button></div>
      <button className={`wb-library-project ${!route.project ? 'selected' : ''}`} aria-pressed={!route.project} onClick={() => filter({ project: '' })}><Icon name="history" /><span>全部项目</span></button>
      <button className={`wb-library-project ${route.project === 'unassigned' ? 'selected' : ''}`} aria-pressed={route.project === 'unassigned'} onClick={() => filter({ project: 'unassigned' })}><Icon name="chat" /><span>未归类会话</span>{projects.page?.unassignedRecords !== undefined && <em>{projects.page.unassignedRecords} 条记录</em>}</button>
      <form className="wb-library-project-search" onSubmit={e => { e.preventDefault(); nav({ projectQ: projectDraft.trim() }); }}><input type="search" aria-label="搜索项目" placeholder="搜索项目名称或路径" maxLength={512} value={projectDraft} onInput={e => setProjectDraft(e.currentTarget.value)} /><button type="submit">查找项目</button></form>
      <p className="wb-library-project-order">最近更新</p>
      <div className="wb-library-project-list">
        {projects.page?.records.filter(project => project.projectId).map(project => <div className="wb-library-project-item" key={project.projectId!}><button className={`wb-library-project ${route.project === project.projectId ? 'selected' : ''}`} aria-pressed={route.project === project.projectId} onClick={() => filter({ project: project.projectId! })}><Icon name={project.path ? 'folder' : 'unavailable'} /><span><strong>{projectName(project.path)}</strong>{project.path && <small>{project.path}</small>}</span><em title={`${project.entries} 条来源记录，不代表唯一对话数`}>{project.entries}</em></button>{project.path && <details className="wb-library-project-path"><summary aria-label={`完整路径 · ${projectName(project.path)}`} title="展开项目完整路径">⋯</summary><p>{project.path}</p></details>}</div>)}
        {projects.busy && <p role="status" className="wb-subtle">正在读取项目…</p>}{projects.error && <p role="alert" className="wb-notice">{projects.error}<button onClick={projects.reset}>重新读取</button></p>}
      </div>
      {(projects.index > 0 || projects.page?.nextCursor) && pageControls(projects, '项目分页')}
      <details className="wb-library-sources"><summary>历史来源 · {sources.length}</summary><ul className="wb-home-sources">{sources.map(source => <li key={source.id}><strong>{sourceNames[source.kind] || '历史来源'}<small>{source.id}</small></strong><span role={source.state === 'unavailable' ? 'status' : undefined}>{source.message}</span></li>)}</ul></details>
    </aside>
    <section className="wb-library-main">
      <div className="wb-library-toolbar"><button className="wb-library-project-toggle" onClick={() => setMobileProjects(v => !v)} aria-expanded={mobileProjects}><Icon name="folder" /> 项目</button><h1>{pageTitle}</h1>{route.project && route.project !== 'unassigned' && onProjectLaunch && (activeRun ? <a className="wb-library-primary-action" href={`/?run=${encodeURIComponent(activeRun.runId)}`}>进入工作台</a> : <button className="wb-library-primary-action" disabled={!projectPath || selectedProject?.status !== 'available'} onClick={() => onProjectLaunch(route.project)}>开始新对话</button>)}<button onClick={() => void update()} title="重新检查已登记的来源"><Icon name="refresh" /> 更新历史</button></div>
      <div className="wb-library-filters"><form onSubmit={e => { e.preventDefault(); filter({ q: draft.trim() }); }}><Icon name="search" /><input type="search" aria-label="搜索会话和消息" placeholder="搜索会话和消息" maxLength={512} value={draft} onInput={e => setDraft(e.currentTarget.value)} /><button type="submit">搜索</button></form><select aria-label="历史来源筛选" value={route.source} onChange={e => filter({ source: e.currentTarget.value })}><option value="">全部来源</option>{sources.map(s => <option key={s.id} value={s.id}>{sourceNames[s.kind] || '历史'}{sources.filter(other => other.kind === s.kind).length > 1 ? ` · 来源 ${sources.filter(other => other.kind === s.kind).findIndex(other => other.id === s.id) + 1}` : ''}</option>)}</select><select aria-label="历史展示方式" value={view} onChange={e => filter({ view: e.currentTarget.value, section: '', group: '' })}><option value="groups">整理视图</option><option value="records">全部来源记录</option></select>{view === 'records' && <select aria-label="会话分组" value={route.group} onChange={e => filter({ group: e.currentTarget.value })}><option value="">全部会话</option><option value="main">主会话</option><option value="agents">子代理</option></select>}</div>
      {notice && <div className={`wb-library-update${notice.kind === 'failed' ? ' is-failed' : ''}`} role={notice.kind === 'failed' ? 'alert' : 'status'} data-update-state={notice.kind}><span>{notice.text}</span>{notice.kind === 'available' && <button onClick={() => { setRefresh(n => n + 1); setNotice(null); }}>载入新结果</button>}{notice.kind === 'failed' && <button onClick={() => void update()}>重试更新</button>}</div>}
      <div className="wb-library-entry-list" ref={list} hidden={!!route.entry}>
        {route.q && <div className="wb-library-list-heading"><strong>搜索“{route.q}”</strong><button onClick={() => filter({ q: '' })}>清除“{route.q}”</button></div>}
        {route.project && route.project !== 'unassigned' && projectPath && <details className="wb-library-selected-path"><summary>项目完整路径</summary><p>{projectPath}</p>{selectedProject?.status === 'missing' && <p>项目目录不存在，历史仍可阅读，不能开始新对话。</p>}</details>}
        <div className="wb-library-section-top"><h2 className="wb-library-section-heading">{view === 'groups' ? '主要会话' : '来源记录'}</h2>{entries.page && <details className="wb-library-count-details"><summary>数量与来源</summary>{entries.page.totalRecords !== undefined && <p className="wb-library-count">当前筛选 · {entries.page.totalRecords} 条来源记录</p>}{entries.page.counts && <p>主要会话 · {entries.page.counts.primary.groups} 个展示组 · {entries.page.counts.primary.records} 条来源记录<br />辅助会话 · {entries.page.counts.auxiliary.groups} 个展示组 · {entries.page.counts.auxiliary.records} 条来源记录<br />运行记录 · {entries.page.counts.runs.groups} 个展示组 · {entries.page.counts.runs.records} 条运行记录</p>}<p>数量按当前筛选计算；来源记录不等于唯一对话数。</p></details>}</div>
        {entries.page && coverageText(entries.page.coverage) && <p className={`wb-library-coverage${coverageText(entries.page.coverage) === '搜索只覆盖已索引的内容；打开会话可按需读取正文。' ? ' is-routine' : ''}`}>{coverageText(entries.page.coverage)}</p>}
        {entries.error && <p role="alert" className="wb-notice">{entries.error}<button onClick={entries.reset}>重新读取列表</button></p>}
        {entries.busy && <p role="status" className="wb-subtle">正在读取历史…</p>}
        {view === 'groups' && entries.page && <SessionGroupList groups={visibleGroups} revision={entries.page.revision} sources={sources} onOpen={open} showProject={showEntryProject} />}
        {visibleEntries.map(entry => <article key={entry.entryId} className="wb-library-session-group"><button data-entry={entry.entryId} className="wb-library-entry" onClick={() => open(entry)}>
          <span className="wb-library-entry-icon"><Icon name={entry.isSubagent || entry.parentThreadId ? 'git' : 'chat'} /></span><span className="wb-library-entry-text"><strong>{entry.title}</strong><ExchangePreview preview={entry.preview} />{entry.match && <small className="wb-library-hit">搜索命中：{entry.match.text}</small>}{showEntryProject && <span className="wb-library-entry-project">{entry.projectId ? projectName(entry.projectPath) : '未归类会话'}</span>}</span><time>{dateLabel(entry.recordedAt)}</time><Icon name="chevron" />
        </button><details className="wb-library-entry-info"><summary aria-label={`记录信息 · ${entry.title}`} title="查看来源与记录信息">⋯</summary><div className="wb-library-group-sources"><p>{sourceNames[entry.kind] || '历史记录'}{(entry.isSubagent || entry.parentThreadId) && ` · 子代理 ${entry.agentName || ''}`}</p><p>项目路径：{entry.projectPath || '未归类，工作目录可在记录详情中查看'}</p></div></details></article>)}
        {!entries.busy && !entries.error && !visibleCount && <div className="wb-library-empty"><Icon name="history" /><h2>{pendingCatalog ? '正在整理历史' : sources.every(s => s.state === 'unavailable') ? '历史来源暂不可用' : view === 'groups' ? route.q ? '未在主要会话中找到匹配' : '暂无主要会话' : route.q || route.project || route.source || route.group ? '没有匹配的记录' : '这里还没有历史记录'}</h2><p>{pendingCatalog ? indexedEntries > 0 ? `已整理 ${indexedEntries} 条记录，目录仍在更新，完成的记录会自动显示。` : '目录仍在更新，完成的记录会自动显示。' : sources.some(s => s.state === 'unavailable') ? '部分历史位置无法读取。展开“历史来源”或设置检查已登记的位置。' : view === 'groups' ? '辅助会话与运行记录可从下方入口查看；也可切换全部来源记录核查。' : '可在设置中接入其它历史来源，或调整当前筛选。'}</p></div>}
        {(entries.index > 0 || entries.page?.nextCursor) && pageControls(entries, '会话分页')}
        {view === 'groups' && <div className="wb-library-extra-sections">{(['auxiliary', 'runs'] as const).map(value => <div key={value}><button className="wb-library-section-toggle" aria-expanded={section === value} onClick={() => filter({ section: section === value ? 'primary' : value })}>{value === 'auxiliary' ? '辅助会话' : '运行记录'}</button>{section === value && <section aria-label={value === 'auxiliary' ? '辅助会话' : '运行记录'}>{extra.busy && <p role="status">正在读取…</p>}{extra.error && <p role="alert">{extra.error}<button onClick={extra.reset}>重新读取列表</button></p>}{extraMismatch && <p className="wb-library-note">目录版本已变化，请载入新结果后查看。</p>}{!extraMismatch && extra.page && <><p className="wb-library-note">{coverageText(extra.page.coverage)}</p><SessionGroupList groups={extra.page.records} revision={extra.page.revision} sources={sources} onOpen={open} showProject={showEntryProject} />{!extra.busy && !extra.page.records.length && <p>当前筛选没有此类记录。</p>}</>}{!extraMismatch && (extra.index > 0 || extra.page?.nextCursor) && pageControls(extra, value === 'auxiliary' ? '辅助分页' : '运行分页')}</section>}</div>)}</div>}
      </div>
      {route.entry && <LibraryReader key={`${route.entry}:${route.revision}:${route.match}`} instanceId={instanceId} onOrganizationSaved={() => { organizationReading.current = route.entry; setRefresh(value => value + 1); setNotice({ kind: 'info', text: '已核对当前项目归属，目录和计数重新读取。当前来源正文与阅读位置保持不变。' }); }} route={route} sources={sources} onResume={onResume} onBack={() => nav({ entry: '', revision: '', match: '' })} onEntry={id => nav({ entry: id, revision: '', match: '' })} />}
    </section>
  </main>;
}
