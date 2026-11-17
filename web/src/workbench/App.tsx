import { workbenchFetch } from './context';
import { Fragment } from 'preact';
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { ModelMessage } from './ModelMessage';
import { TerminalPanel } from './TerminalPanel';
import { RunEndedNotice, type TerminalExit } from './RunEndedNotice';
import { UserMessage } from './UserMessage';
import { ToolCard } from './ToolCard';
import { conversationEntries, nativeDirect, useReading, type TextItem } from './reading';
import { readAnchor, restoreAnchor, type ReadingAnchor } from './readingAnchor';
import { userMessages } from './viewItems';
import { CallInspector, type OpenCalls, type SavedCallSource } from './CallInspector';
import { HistoryPanel } from './HistoryPanel';
import { SaveOverview, UsageFooter, UsageOverview } from './UsageOverview';
import { RunDiagnostics } from './RunDiagnostics';
import { AccessPanel } from './AccessPanel';
import { SettingsPanel } from './SettingsPanel';
import { WorkspacePanel } from './WorkspacePanel';
import { FileReading } from './FileReading';
import { WorkspaceLinks } from './WorkspaceLinks';
import type { FileSelection, WorkspaceTab } from './workspace';
import { Icon } from './Icons';
import { runApi } from './runApi';
import { navigatePage, PageLink } from './navigation';
import { runPath, type RunPage } from './routes';
import { DockSeparator } from './DockSeparator';

export interface Run { runEpoch: string; terminalAvailable: boolean; projectName: string | null; processId: number | null; accessAvailable?: boolean; historyAvailable?: boolean; settingsAvailable?: boolean; historyManagementAvailable?: boolean; workspaceRoot?: string | null }


export function App({ run, applicationHome = false, page, framed = false }: { run: Run; applicationHome?: boolean; page?: RunPage; framed?: boolean }) {
  const shell = useRef<HTMLDivElement>(null);
  const options = useRef<HTMLDetailsElement>(null);
  const fileTrigger = useRef<HTMLElement | null>(null);
  const routeTo = (next: RunPage) => { setInspection(null); navigatePage(runPath(run.runEpoch, next)); };
  useEffect(() => { document.title = `${run.projectName || '当前项目'} · Codex 工作台`; }, [run.projectName]);
  const reading = useReading(run.runEpoch);
  const users = userMessages(reading);
  const notices = reading.items.filter(item => item.kind === 'notice');
  const chat = conversationEntries(reading);
  const responseFor = (item: TextItem) => reading.responses.find(response => response.requestId === item.key.requestId && response.responseId === item.key.responseId);
  const [legacyCenter, setCenter] = useState<'conversation' | 'history' | 'file'>('conversation');
  const [legacySidebar, setSidebar] = useState<'run' | WorkspaceTab>('run');
  const [legacyWorkspaceTab, setWorkspaceTab] = useState<WorkspaceTab>('files');
  const workspacePage = page === 'files' || page === 'search' || page === 'git';
  const priorCenter = useRef<'conversation' | 'history'>('conversation');
  const unavailablePage = (workspacePage && !run.workspaceRoot) || (page === 'history' && !run.historyAvailable) || (page === 'settings' && !run.settingsAvailable) || (page === 'access' && !run.accessAvailable);
  const center = page ? page === 'history' ? 'history' : workspacePage ? framed ? priorCenter.current : 'file' : 'conversation' : legacyCenter;
  const sidebar = page ? workspacePage ? page : 'run' : legacySidebar;
  const workspaceTab = workspacePage ? page : legacyWorkspaceTab;
  const [file, setFile] = useState<FileSelection | null>(null);
  const openFile = (next: FileSelection) => { if (center !== 'file') priorCenter.current = center; if (document.activeElement instanceof HTMLElement && !document.activeElement.closest('.wb-file-reading')) fileTrigger.current = document.activeElement; setInspection(null); setFile(next); if ((page || framed) && !workspacePage) routeTo('files'); else if (!framed) setCenter('file'); if (!page && window.innerWidth <= 900) setSidebar('run'); };
  const closeFile = () => { setFile(null); requestAnimationFrame(() => { if (fileTrigger.current?.isConnected) fileTrigger.current.focus({ preventScroll: true }); }); };
  const openWorkspace = (tab: WorkspaceTab) => { if (page) routeTo(tab); else { setWorkspaceTab(tab); setSidebar(sidebar === tab ? 'run' : tab); } };
  const [legacyAccessOpen, setAccessOpen] = useState(false);
  const accessOpen = page ? page === 'access' : legacyAccessOpen;
  const accessButton = useRef<HTMLButtonElement>(null);
  const closeAccess = () => { if (page) routeTo('chat'); else setAccessOpen(false); if (framed) options.current?.querySelector('summary')?.focus({ preventScroll: true }); else accessButton.current?.focus({ preventScroll: true }); };
  const [legacySettingsOpen, setSettingsOpen] = useState(false);
  const settingsOpen = page ? page === 'settings' : legacySettingsOpen;
  const [settingsDirty, setSettingsDirty] = useState(false);
  const settingsButton = useRef<HTMLButtonElement>(null);
  const closeSettings = useCallback((restoreFocus: boolean) => { if (page) navigatePage(runPath(run.runEpoch)); else setSettingsOpen(false); if (restoreFocus) { if (framed) options.current?.querySelector('summary')?.focus({ preventScroll: true }); else settingsButton.current?.focus({ preventScroll: true }); } }, [page, run.runEpoch, framed]);
  const [inspection, setInspection] = useState<{ requestId: string | null; source?: SavedCallSource } | null>(null);
  useLayoutEffect(() => { setInspection(null); }, [page]);
  const inspectionTrigger = useRef<HTMLElement | null>(null);
  const restoreInspectionFocus = useRef(false);
  const navigate = (next: 'conversation' | 'history') => { if (page) routeTo(next === 'conversation' ? 'chat' : 'history'); else { setInspection(null); setCenter(next); } };
  const openCalls: OpenCalls = (requestId, source) => {
    inspectionTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetails(false); setInspection({ requestId, source });
  };
  const closeCalls = () => {
    restoreInspectionFocus.current = true;
    setInspection(null);
  };
  const [terminal, setTerminal] = useState(true);
  const [dock, setDock] = useState<'right' | 'bottom'>(() => { try { return localStorage.getItem('codex-view:terminal-dock:v1') === 'bottom' ? 'bottom' : 'right'; } catch { return 'right'; } });
  const [mobileView, setMobileView] = useState<'reading' | 'terminal'>(page === 'terminal' ? 'terminal' : 'reading');
  const [mobileLayout, setMobileLayout] = useState(() => window.innerWidth < 768);
  useEffect(() => {
    const update = () => setMobileLayout(window.innerWidth < 768);
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  }, []);
  const readingVisible = !framed || !mobileLayout || mobileView === 'reading';
  const workspaceActive = workspacePage && readingVisible;
  const [focusRequest, setFocusRequest] = useState(0);
  const columns = useRef<HTMLDivElement>(null);
  const tools = useRef<HTMLElement>(null);
  const closeContext = () => {
    const trigger = [...(tools.current?.querySelectorAll('a') || [])].find(link => new URL(link.href).pathname.endsWith(`/${workspaceTab}`));
    routeTo(priorCenter.current === 'history' ? 'history' : 'chat');
    requestAnimationFrame(() => trigger?.focus({ preventScroll: true }));
  };
  const focusTerminal = () => { setTerminal(true); setMobileView('terminal'); setFocusRequest(value => value + 1); };
  const toggleDock = () => { const next = dock === 'right' ? 'bottom' : 'right'; setDock(next); try { localStorage.setItem('codex-view:terminal-dock:v1', next); } catch { /* A display preference need not be persisted. */ } };
  useLayoutEffect(() => {
    if (page === 'chat' || page === 'history') priorCenter.current = page === 'history' ? 'history' : 'conversation';
    setMobileView(page === 'terminal' ? 'terminal' : 'reading');
    if (options.current) options.current.open = false;
  }, [page]);
  const readingPage = !page || page === 'chat' || page === 'history' || workspacePage;
  const terminalVisible = page === 'terminal' || (run.terminalAvailable && terminal && (readingPage && (framed || !page || page === 'chat')));
  const [exit, setExit] = useState<{ epoch: string; result: TerminalExit } | null>(null);
  const [dismissedExit, setDismissedExit] = useState<string | null>(null);
  const currentExit = exit?.epoch === run.runEpoch ? exit.result : null;
  const onExit = useCallback((result: TerminalExit) => {
    setExit({ epoch: run.runEpoch, result });
    setStopConfirm(false);
  }, [run.runEpoch]);
  const [following, setFollowing] = useState(true);
  const followRef = useRef(true);
  const [legacyDetails, setDetails] = useState(false);
  const details = page ? page === 'usage' && !inspection : legacyDetails;
  const detailsButton = useRef<HTMLButtonElement>(null);
  const callsButton = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    if (inspection || !restoreInspectionFocus.current) return;
    restoreInspectionFocus.current = false;
    const trigger = inspectionTrigger.current;
    if (trigger?.isConnected) trigger.focus();
    else if (page === 'usage') callsButton.current?.focus({ preventScroll: true });
    else detailsButton.current?.focus();
  }, [inspection]);
  const closeDetails = () => { if (page) routeTo('chat'); else setDetails(false); if (framed) shell.current?.querySelector<HTMLButtonElement>('.wb-footer button')?.focus(); else detailsButton.current?.focus(); };
  const [stopping, setStopping] = useState(false);
  const [stopConfirm, setStopConfirm] = useState(false);
  const [stopError, setStopError] = useState('');
  const messages = useRef<HTMLDivElement>(null);
  const anchor = useRef<ReadingAnchor | null>(null);
  useLayoutEffect(() => {
    if (messages.current && !followRef.current && center === 'conversation') restoreAnchor(messages.current, anchor.current);
  }, [reading.viewSeq, center]);
  useEffect(() => {
    if (!following || center !== 'conversation') return;
    const frame = requestAnimationFrame(() => {
      if (messages.current && followRef.current) messages.current.scrollTop = messages.current.scrollHeight;
    });
    return () => cancelAnimationFrame(frame);
  }, [reading.viewSeq, following, center]);
  async function stop() {
    setStopConfirm(false); setStopping(true); setStopError('');
    try {
      const result = await workbenchFetch(runApi('/run/stop'), {
        method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ epoch: run.runEpoch })
      });
      if (!result.ok || (await result.json()).accepted !== true) throw new Error('停止请求未成功，请检查连接。');
    } catch (error) { setStopError((error as Error).message); setStopping(false); }
  }
  return <WorkspaceLinks.Provider value={run.workspaceRoot ? { root: run.workspaceRoot, open: openFile } : null}><div ref={shell} className={`wb-shell ${page ? `wb-routed wb-route-${page}` : ''} ${sidebar !== 'run' ? 'wb-project-open' : ''} ${terminal ? '' : 'wb-terminal-hidden'} ${framed ? `wb-framed wb-dock-${dock} wb-mobile-${mobileView}` : ''}`}>
    {framed ? <header className="wb-session-header">
      <div className="wb-session-heading"><span className="wb-session-breadcrumb"><Icon name="folder" /> 当前会话 <span> / {run.runEpoch.slice(0, 8)}</span></span><div><h1>{run.projectName || '当前项目'}</h1>{currentExit ? <button className="wb-run-ended-status" aria-label="查看运行结束提示" onClick={() => setDismissedExit(null)}>已结束</button> : <span className={`wb-session-sync ${reading.connected ? 'is-live' : ''}`}><span className={`wb-state-dot ${reading.connected ? 'is-live' : 'is-waiting'}`} />{reading.connected ? '阅读已连接' : '阅读连接中'}</span>}</div></div>
      <div className="wb-session-actions">{run.terminalAvailable && (readingPage || page === 'terminal') && <button aria-pressed={terminalVisible} onClick={() => { if (page === 'terminal') { setTerminal(false); routeTo('chat'); } else if (terminal) { setTerminal(false); setMobileView('reading'); } else focusTerminal(); }}><Icon name="terminal" />终端</button>}
        <details ref={options} className="wb-session-options" onKeyDown={event => { if (event.key === 'Escape') { event.preventDefault(); event.currentTarget.open = false; event.currentTarget.querySelector('summary')?.focus(); } }}><summary aria-label="会话选项">•••</summary><nav aria-label="会话选项">{run.terminalAvailable && <PageLink path={runPath(run.runEpoch, 'terminal')}>独立终端</PageLink>}<PageLink path={runPath(run.runEpoch, 'usage')}>用量与状态</PageLink>{run.settingsAvailable && <PageLink path={runPath(run.runEpoch, 'settings')}>运行设置{settingsDirty ? ' · 未保存' : ''}</PageLink>}{run.accessAvailable && <PageLink path={runPath(run.runEpoch, 'access')}>本地局域网接入</PageLink>}</nav></details>
      </div>
    </header> : <header className="wb-header">{applicationHome && <PageLink className="wb-home-link" path="/">← {page ? '项目概览' : '全部历史'}</PageLink>}<span className="wb-brand">›_</span><strong>{run.projectName || '当前项目'}</strong><span className="wb-subtle">Codex 工作台</span>
      <div className="wb-header-actions"><span className="wb-run-state">{currentExit
        ? <button className="wb-run-ended-status" aria-label="查看运行结束提示" onClick={() => setDismissedExit(null)}><span aria-hidden="true">■</span> 已结束</button>
        : <span className={`wb-connection ${reading.connected ? 'is-live' : ''}`}>{reading.connected ? '已连接' : '连接中'}</span>}</span>
        {!page && run.accessAvailable && <button ref={accessButton} className="wb-access-trigger" aria-expanded={accessOpen} aria-controls="wb-access-panel" onClick={() => { setSettingsOpen(false); setDetails(false); setAccessOpen(!accessOpen); }}><svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><rect x="4" y="1.5" width="8" height="13" rx="1.5" /><path d="M6.5 3.5h3M7 12.5h2" /></svg>本地局域网接入</button>}
        {!page && run.settingsAvailable && <button id="wb-settings-button" ref={settingsButton} aria-expanded={settingsOpen} aria-controls="wb-settings-panel" onClick={() => { setDetails(false); setAccessOpen(false); setSettingsOpen(!settingsOpen); }}>设置{settingsDirty ? ' · 未保存' : ''}</button>}
        {(!page || page === 'chat') && <button aria-pressed={terminal} onClick={() => setTerminal(!terminal)}>{page ? '并排终端' : '终端'}</button>}
      </div>
    </header>}
    {page && !framed && <nav className="wb-page-nav" aria-label="工作台页面">{(['chat', 'terminal', ...(run.workspaceRoot ? ['files', 'search', 'git'] as const : []), ...(run.historyAvailable ? ['history'] as const : []), 'usage', ...(run.settingsAvailable ? ['settings'] as const : []), ...(run.accessAvailable ? ['access'] as const : [])] as RunPage[]).map(value => <Fragment key={value}>{value === 'usage' && <span className="wb-page-nav-divider" aria-hidden="true" />}<PageLink path={runPath(run.runEpoch, value)} active={page === value}>{{ chat: '实时对话', terminal: '原生终端', files: '文件', search: '搜索', git: 'Git', history: '运行历史', usage: '用量与状态', settings: '设置', access: '本地局域网接入' }[value]}{value === 'settings' && settingsDirty ? ' · 未保存' : ''}</PageLink></Fragment>)}</nav>}
    {framed && <nav ref={tools} className="wb-session-tools" aria-label="会话功能"><div><PageLink path={runPath(run.runEpoch)} active={!page || page === 'chat'}><Icon name="chat" />对话</PageLink>{run.historyAvailable && <PageLink path={runPath(run.runEpoch, 'history')} active={page === 'history'}><Icon name="history" />历史</PageLink>}</div>{run.workspaceRoot && <div>{(['files', 'search', 'git'] as const).map(tab => <PageLink key={tab} path={runPath(run.runEpoch, tab)} active={page === tab}><Icon name={tab} />{{ files: '文件', search: '搜索', git: '更改' }[tab]}</PageLink>)}</div>}</nav>}
    {framed && run.terminalAvailable && (readingPage || page === 'terminal') && <div className="wb-mobile-workspace-tabs" role="tablist" aria-label="工作区视图">{(['reading', 'terminal'] as const).map(view => <button key={view} id={`wb-view-${view}`} role="tab" aria-selected={mobileView === view} tabIndex={mobileView === view ? 0 : -1} aria-controls={view === 'reading' ? 'wb-session-reading' : 'wb-session-terminal'} onClick={() => { if (view === 'reading') { if (page === 'terminal') routeTo('chat'); setMobileView('reading'); } else focusTerminal(); }} onKeyDown={event => { if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return; event.preventDefault(); const next = view === 'reading' ? 'terminal' : 'reading'; if (next === 'reading' && page === 'terminal') routeTo('chat'); setTerminal(true); setMobileView(next); document.getElementById(`wb-view-${next}`)?.focus(); }}><Icon name={view === 'reading' ? 'chat' : 'terminal'} />{view === 'reading' ? '阅读' : '终端'}</button>)}</div>}
    {unavailablePage && <p className="wb-notice" role="status">当前运行未提供此功能。请从导航中打开可用页面。</p>}
    {run.accessAvailable && <AccessPanel key={run.runEpoch} epoch={run.runEpoch} open={accessOpen} onClose={closeAccess} />}
    <div className="wb-columns" ref={columns} data-terminal-visible={terminalVisible} hidden={unavailablePage || (!!page && (page === 'access' || (page === 'usage' && !inspection)))}>
      <nav className="wb-activity" aria-label="工作台导航" hidden={!!page}>
        <button aria-label="实时对话" title="实时对话" aria-current={center === 'conversation' ? 'page' : undefined} className={center === 'conversation' ? 'selected' : ''} onClick={() => navigate('conversation')}><Icon name="chat" /><span>对话</span></button>
        {run.historyAvailable && <button aria-label="历史记录" title="历史记录" aria-current={center === 'history' ? 'page' : undefined} className={center === 'history' ? 'selected' : ''} onClick={() => navigate('history')}><Icon name="history" /><span>历史</span></button>}
        {run.workspaceRoot && <div className="wb-activity-project">{(['files', 'search', 'git'] as const).map(tab => <button key={tab} aria-label={{ files: '文件', search: '搜索代码', git: 'Git' }[tab]} title={{ files: '浏览项目文件', search: '搜索项目代码', git: 'Git 变更与提交记录' }[tab]} aria-expanded={sidebar === tab} className={sidebar === tab ? 'selected' : ''} onClick={() => openWorkspace(tab)}><Icon name={tab} /><span>{{ files: '文件', search: '搜索', git: 'Git' }[tab]}</span></button>)}</div>}
        <button ref={detailsButton} className={`wb-activity-usage ${details ? 'selected' : ''}`} aria-label="用量与状态" title="Token 用量与保存状态" aria-expanded={details} aria-controls="wb-run-overview" onClick={() => { setInspection(null); setDetails(!details); }}><Icon name="usage" /><span>用量</span></button>
      </nav>
      <aside className="wb-sidebar" hidden={framed || (!!page && !workspacePage)}><div hidden={sidebar !== 'run'} className="wb-run-sidebar"><div className="wb-panel-heading">当前运行</div><div className="wb-sidebar-content">
        <span className="wb-section-label">项目</span><strong>{run.projectName || '当前目录'}</strong>
        <button aria-label="在中央查看实时对话" className={center === 'conversation' ? 'active' : ''} onClick={() => navigate('conversation')}><Icon name="chat" />实时对话 <span>{chat.length}</span></button>
        {run.historyAvailable && <button aria-label="在中央查看历史记录" className={center === 'history' ? 'active' : ''} onClick={() => navigate('history')}><Icon name="history" />历史记录</button>}
        <p className="wb-subtle">{currentExit ? '本次运行已结束，可继续查看对话与终端记录。' : '在右侧原生终端输入，中央区分用户提交与模型回复。'}</p>
        {run.historyAvailable && <div className="wb-sidebar-note">在历史记录中查看本项目已保存的对话。</div>}
      </div></div>{!framed && run.workspaceRoot && <WorkspacePanel tab={workspaceTab} active={sidebar !== 'run'} projectName={run.projectName} selection={center === 'file' ? file : null} onClose={() => page ? routeTo('chat') : setSidebar('run')} onOpen={openFile} />}</aside>
      <main id="wb-session-reading" className="wb-reading" hidden={page === 'terminal'}>
        <div className="wb-panel-heading" hidden={framed || page === 'settings'}><span>{center === 'file' ? '项目阅读' : center === 'conversation' ? '实时对话' : '历史记录'}</span><span className="wb-subtle">{currentExit ? '本次运行已结束 · 内容可查看' : center === 'history' ? '内容只读 · 当前 CLI 保持运行' : '当前运行'}</span></div>
        <div className="wb-message-list" ref={messages} hidden={center !== 'conversation' || (!!page && page !== 'chat' && (!framed || !workspacePage))} inert={!!inspection || settingsOpen} tabIndex={framed ? 0 : undefined} onScroll={() => {
          const element = messages.current;
          if (element) {
            anchor.current = readAnchor(element);
            followRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 70;
            setFollowing(followRef.current);
          }
        }}>
          {!chat.length && <div className="wb-empty"><span aria-hidden="true">{currentExit ? '■' : '✳'}</span><h1>{currentExit ? '本次运行已结束' : nativeDirect(reading) ? '原生直连模式' : '等待对话回复'}</h1><p>{nativeDirect(reading) ? '模型实时捕获不可用。请在原生终端查看回复；原生历史仍可从首页阅读。' : currentExit ? '本次没有捕获到可展示的对话，终端输出仍可查看。' : '在右侧终端直接输入，即可使用原生 Codex。'}{!nativeDirect(reading) && <><br />调用记录可从左下角的用量概览中查看。</>}</p></div>}
          {notices.filter(item => item.evidence.length === 0).map(item => <p key={item.itemKey} className="wb-notice" role="status">{item.text}</p>)}
          {chat.map(entry => <div key={entry.key} data-reading-key={entry.key}>{entry.kind === 'user'
            ? <UserMessage user={entry.user} orderUnconfirmed={entry.orderUnconfirmed} />
            : entry.kind === 'tool' ? <ToolCard tool={entry.tool} onInspect={() => openCalls(entry.tool.key.requestId)} />
            : <ModelMessage onInspect={() => openCalls(entry.item.key.requestId)} item={entry.item} response={responseFor(entry.item)} request={entry.request} userPending={!users.some(user => user.key.codexThreadId === entry.request.codexThreadId && user.key.codexTurnId === entry.request.codexTurnId)} />}</div>)}
        </div>
        {run.historyAvailable && <HistoryPanel epoch={run.runEpoch} active={center === 'history' && readingVisible} managementAvailable={run.historyManagementAvailable} onSettings={run.settingsAvailable ? () => { if (page) routeTo('settings'); else { setDetails(false); setAccessOpen(false); setSettingsOpen(true); } } : undefined} blocked={!!inspection || settingsOpen} onReturn={() => navigate('conversation')} onInspect={openCalls} onSelectionChange={() => setInspection(current => current?.source ? null : current)} />}
        {!framed && center === 'file' && file && <FileReading selection={file} onOpen={openFile} onClose={() => page ? setFile(null) : setCenter(priorCenter.current)} blocked={!!inspection || settingsOpen} />}
        {!framed && workspacePage && !file && <div className="wb-empty"><span aria-hidden="true">{{ files: '◱', search: '⌕', git: '⎇' }[page]}</span><h1>{{ files: '浏览项目文件', search: '搜索项目代码', git: '查看项目变更' }[page]}</h1><p>从左侧选择文件，在这里阅读内容。</p></div>}
        {framed && <div className="wb-reading-continue" hidden={settingsOpen || !readingPage}><span>{center === 'history' ? '历史只读 · 终端仍属于当前运行' : currentExit ? '本次运行已结束' : nativeDirect(reading) ? '原生直连 · 在终端查看响应' : '实时阅读 · 输入与审批在原生终端中完成'}</span>{run.terminalAvailable && <button onClick={focusTerminal}>在终端继续 <span aria-hidden="true">→</span></button>}</div>}
        {framed && run.workspaceRoot && <section className="wb-session-context" aria-label="会话辅助面板" hidden={!workspacePage} inert={!!inspection || settingsOpen} onKeyDown={event => { if (event.key === 'Escape' && !event.defaultPrevented) { event.preventDefault(); closeContext(); } }}><header><strong>{{ files: '项目文件', search: '代码搜索', git: 'Git 更改' }[workspaceTab]}</strong><button aria-label="关闭辅助面板" onClick={closeContext}><Icon name="close" /></button></header><div className={`wb-session-context-content ${file ? 'has-file' : ''}`}><WorkspacePanel tab={workspaceTab} active={workspaceActive} projectName={run.projectName} selection={file} onClose={closeContext} onOpen={openFile} />{file ? <FileReading selection={file} onOpen={openFile} onClose={closeFile} blocked={!!inspection || settingsOpen} active={workspaceActive} /> : <div className="wb-context-empty"><Icon name={workspaceTab} /><span>选择文件，在会话旁边阅读内容。</span></div>}</div></section>}
        {inspection && <CallInspector blocked={settingsOpen} reading={inspection.source?.reading || reading} epoch={inspection.source?.epoch || run.runEpoch} historical={!!inspection.source} before={inspection.source?.before} requestId={inspection.requestId} onSelect={requestId => setInspection({ ...inspection, requestId })} onClose={closeCalls} />}
        {run.settingsAvailable && <SettingsPanel key={run.runEpoch} epoch={run.runEpoch} embedded={!!page} open={settingsOpen} managementAvailable={run.historyManagementAvailable} onClose={closeSettings} onDirtyChange={setSettingsDirty} onViewHistory={() => { closeSettings(false); navigate('history'); }} />}
        {!following && center === 'conversation' && !inspection && <button className="wb-follow" onClick={() => { followRef.current = true; setFollowing(true); }}>↓ 跟随最新</button>}
      </main>
      {framed && <DockSeparator container={columns} dock={dock} hidden={!terminalVisible || page === 'terminal'} />}
      <div id="wb-session-terminal" className="wb-terminal-container" hidden={!terminalVisible}>{run.terminalAvailable ? <TerminalPanel key={run.runEpoch} epoch={run.runEpoch} onExit={onExit} focusRequest={focusRequest} actions={framed ? <><button className="wb-terminal-position" aria-label={dock === 'right' ? '切换为底部终端' : '切换为右侧终端'} onClick={toggleDock}><Icon name="dock" /></button><button aria-label="收起终端" onClick={() => { setTerminal(false); setMobileView('reading'); if (page === 'terminal') routeTo('chat'); }}><Icon name="close" /></button></> : undefined} /> : page === 'terminal' && <p className="wb-notice">这次运行没有可用的原生终端。</p>}</div>
    </div>
    {details && <section id="wb-run-overview" className="wb-status-details" aria-label="用量与状态" onKeyDown={event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); closeDetails(); }
    }}>
      <div className="wb-panel-heading"><strong>用量概览</strong><button onClick={closeDetails} aria-label="关闭用量概览">×</button></div>
      <UsageOverview summary={reading.usageSummary} connected={reading.connected} captureUnavailable={nativeDirect(reading)} />
      <button ref={callsButton} className="wb-view-calls" onClick={() => openCalls(null)}>查看调用记录 <span aria-hidden="true">→</span></button>
      <SaveOverview reading={reading} />
      <RunDiagnostics reading={reading} epoch={run.runEpoch} processId={run.processId} />
      {stopConfirm ? <div className="wb-confirm" role="alertdialog" aria-label="确认停止运行"><p>将停止当前原生 CLI 和正在执行的任务。内存阅读仍可查看。</p><button onClick={() => void stop()}>确认停止</button><button onClick={() => setStopConfirm(false)}>取消</button></div>
        : <button disabled={stopping || !!currentExit} onClick={() => setStopConfirm(true)}>{currentExit ? '本次运行已结束' : stopping ? '已请求停止' : '停止当前运行'}</button>}
      {stopError && <p role="status">{stopError}</p>}
    </section>}
    <UsageFooter reading={reading} expanded={details} onToggle={() => { if (page) routeTo(page === 'usage' ? 'chat' : 'usage'); else { setInspection(null); setDetails(!details); } }} />
    {currentExit && dismissedExit !== run.runEpoch && <RunEndedNotice projectName={run.projectName} exit={currentExit} applicationHome={applicationHome} onClose={() => setDismissedExit(run.runEpoch)} />}
  </div></WorkspaceLinks.Provider>;
}
