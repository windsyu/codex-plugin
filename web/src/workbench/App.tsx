import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks';
import { ModelMessage } from './ModelMessage';
import { TerminalPanel } from './TerminalPanel';
import { UserMessage } from './UserMessage';
import { ToolCard } from './ToolCard';
import { conversationEntries, useReading, type TextItem } from './reading';
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

export interface Run { runEpoch: string; terminalAvailable: boolean; projectName: string | null; processId: number | null; accessAvailable?: boolean; historyAvailable?: boolean; settingsAvailable?: boolean; historyManagementAvailable?: boolean; workspaceRoot?: string | null }


export function App({ run }: { run: Run }) {
  const reading = useReading(run.runEpoch);
  const users = userMessages(reading);
  const notices = reading.items.filter(item => item.kind === 'notice');
  const chat = conversationEntries(reading);
  const responseFor = (item: TextItem) => reading.responses.find(response => response.requestId === item.key.requestId && response.responseId === item.key.responseId);
  const [center, setCenter] = useState<'conversation' | 'history' | 'file'>('conversation');
  const [sidebar, setSidebar] = useState<'run' | WorkspaceTab>('run');
  const [workspaceTab, setWorkspaceTab] = useState<WorkspaceTab>('files');
  const [file, setFile] = useState<FileSelection | null>(null);
  const priorCenter = useRef<'conversation' | 'history'>('conversation');
  const openFile = (next: FileSelection) => { if (center !== 'file') priorCenter.current = center; setInspection(null); setFile(next); setCenter('file'); if (window.innerWidth <= 900) setSidebar('run'); };
  const openWorkspace = (tab: WorkspaceTab) => { setWorkspaceTab(tab); setSidebar(sidebar === tab ? 'run' : tab); };
  const [accessOpen, setAccessOpen] = useState(false);
  const accessButton = useRef<HTMLButtonElement>(null);
  const closeAccess = () => { setAccessOpen(false); accessButton.current?.focus({ preventScroll: true }); };
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsDirty, setSettingsDirty] = useState(false);
  const settingsButton = useRef<HTMLButtonElement>(null);
  const closeSettings = useCallback((restoreFocus: boolean) => { setSettingsOpen(false); if (restoreFocus) settingsButton.current?.focus({ preventScroll: true }); }, []);
  const [inspection, setInspection] = useState<{ requestId: string | null; source?: SavedCallSource } | null>(null);
  const inspectionTrigger = useRef<HTMLElement | null>(null);
  const restoreInspectionFocus = useRef(false);
  const navigate = (next: 'conversation' | 'history') => { setInspection(null); setCenter(next); };
  const openCalls: OpenCalls = (requestId, source) => {
    inspectionTrigger.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setDetails(false); setInspection({ requestId, source });
  };
  const closeCalls = () => {
    restoreInspectionFocus.current = true;
    setInspection(null);
  };
  const [terminal, setTerminal] = useState(true);
  const [following, setFollowing] = useState(true);
  const followRef = useRef(true);
  const [details, setDetails] = useState(false);
  const detailsButton = useRef<HTMLButtonElement>(null);
  useLayoutEffect(() => {
    if (inspection || !restoreInspectionFocus.current) return;
    restoreInspectionFocus.current = false;
    const trigger = inspectionTrigger.current;
    if (trigger?.isConnected) trigger.focus(); else detailsButton.current?.focus();
  }, [inspection]);
  const closeDetails = () => { setDetails(false); detailsButton.current?.focus(); };
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
      const result = await fetch('/workbench/v1/run/stop', {
        method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ epoch: run.runEpoch })
      });
      if (!result.ok) throw new Error('停止请求未成功，请检查连接。');
    } catch (error) { setStopError((error as Error).message); setStopping(false); }
  }
  return <WorkspaceLinks.Provider value={run.workspaceRoot ? { root: run.workspaceRoot, open: openFile } : null}><div className={`wb-shell ${sidebar !== 'run' ? 'wb-project-open' : ''} ${terminal ? '' : 'wb-terminal-hidden'}`}>
    <header className="wb-header"><span className="wb-brand">›_</span><strong>{run.projectName || '当前项目'}</strong><span className="wb-subtle">Codex 工作台</span>
      <div className="wb-header-actions"><span className={`wb-connection ${reading.connected ? 'is-live' : ''}`}>{reading.connected ? '已连接' : '连接中'}</span>
        {run.accessAvailable && <button ref={accessButton} className="wb-access-trigger" aria-expanded={accessOpen} aria-controls="wb-access-panel" onClick={() => { setSettingsOpen(false); setDetails(false); setAccessOpen(!accessOpen); }}><svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" aria-hidden="true"><rect x="4" y="1.5" width="8" height="13" rx="1.5" /><path d="M6.5 3.5h3M7 12.5h2" /></svg>手机接入</button>}
        {run.settingsAvailable && <button id="wb-settings-button" ref={settingsButton} aria-expanded={settingsOpen} aria-controls="wb-settings-panel" onClick={() => { setDetails(false); setAccessOpen(false); setSettingsOpen(!settingsOpen); }}>设置{settingsDirty ? ' · 未保存' : ''}</button>}
        <button aria-pressed={terminal} onClick={() => setTerminal(!terminal)}>终端</button>
      </div>
    </header>
    {run.accessAvailable && <AccessPanel key={run.runEpoch} epoch={run.runEpoch} open={accessOpen} onClose={closeAccess} />}
    <div className="wb-columns">
      <nav className="wb-activity" aria-label="工作台导航">
        <button aria-label="实时对话" title="实时对话" aria-current={center === 'conversation' ? 'page' : undefined} className={center === 'conversation' ? 'selected' : ''} onClick={() => navigate('conversation')}><Icon name="chat" /><span>对话</span></button>
        {run.historyAvailable && <button aria-label="历史记录" title="历史记录" aria-current={center === 'history' ? 'page' : undefined} className={center === 'history' ? 'selected' : ''} onClick={() => navigate('history')}><Icon name="history" /><span>历史</span></button>}
        {run.workspaceRoot && <div className="wb-activity-project">{(['files', 'search', 'git'] as const).map(tab => <button key={tab} aria-label={{ files: '文件', search: '搜索代码', git: 'Git' }[tab]} title={{ files: '浏览项目文件', search: '搜索项目代码', git: 'Git 变更与提交记录' }[tab]} aria-expanded={sidebar === tab} className={sidebar === tab ? 'selected' : ''} onClick={() => openWorkspace(tab)}><Icon name={tab} /><span>{{ files: '文件', search: '搜索', git: 'Git' }[tab]}</span></button>)}</div>}
        <button ref={detailsButton} className={`wb-activity-usage ${details ? 'selected' : ''}`} aria-label="用量与状态" title="Token 用量与保存状态" aria-expanded={details} aria-controls="wb-run-overview" onClick={() => { setInspection(null); setDetails(!details); }}><Icon name="usage" /><span>用量</span></button>
      </nav>
      <aside className="wb-sidebar"><div hidden={sidebar !== 'run'} className="wb-run-sidebar"><div className="wb-panel-heading">当前运行</div><div className="wb-sidebar-content">
        <span className="wb-section-label">项目</span><strong>{run.projectName || '当前目录'}</strong>
        <button aria-label="在中央查看实时对话" className={center === 'conversation' ? 'active' : ''} onClick={() => navigate('conversation')}><Icon name="chat" />实时对话 <span>{chat.length}</span></button>
        {run.historyAvailable && <button aria-label="在中央查看历史记录" className={center === 'history' ? 'active' : ''} onClick={() => navigate('history')}><Icon name="history" />历史记录</button>}
        <p className="wb-subtle">在右侧原生终端输入，中央区分用户提交与模型回复。</p>
        {run.historyAvailable && <div className="wb-sidebar-note">在历史记录中查看本项目已保存的对话。</div>}
      </div></div>{run.workspaceRoot && <WorkspacePanel tab={workspaceTab} active={sidebar !== 'run'} projectName={run.projectName} selection={center === 'file' ? file : null} onClose={() => setSidebar('run')} onOpen={openFile} />}</aside>
      <main className="wb-reading">
        <div className="wb-panel-heading"><span>{center === 'file' ? '项目阅读' : center === 'conversation' ? '实时对话' : '历史记录'}</span><span className="wb-subtle">{center === 'history' ? '内容只读 · 当前 CLI 保持运行' : '当前运行'}</span></div>
        <div className="wb-message-list" ref={messages} hidden={center !== 'conversation'} inert={!!inspection || settingsOpen} onScroll={() => {
          const element = messages.current;
          if (element) {
            anchor.current = readAnchor(element);
            followRef.current = element.scrollHeight - element.scrollTop - element.clientHeight < 70;
            setFollowing(followRef.current);
          }
        }}>
          {!chat.length && <div className="wb-empty"><span>✳</span><h1>等待对话回复</h1><p>在右侧终端直接输入，即可使用原生 Codex。<br />调用记录可从左下角的用量概览中查看。</p></div>}
          {notices.filter(item => item.evidence.length === 0).map(item => <p key={item.itemKey} className="wb-notice" role="status">{item.text}</p>)}
          {chat.map(entry => <div key={entry.key} data-reading-key={entry.key}>{entry.kind === 'user'
            ? <UserMessage user={entry.user} orderUnconfirmed={entry.orderUnconfirmed} />
            : entry.kind === 'tool' ? <ToolCard tool={entry.tool} onInspect={() => openCalls(entry.tool.key.requestId)} />
            : <ModelMessage onInspect={() => openCalls(entry.item.key.requestId)} item={entry.item} response={responseFor(entry.item)} request={entry.request} userPending={!users.some(user => user.key.codexThreadId === entry.request.codexThreadId && user.key.codexTurnId === entry.request.codexTurnId)} />}</div>)}
        </div>
        {run.historyAvailable && <HistoryPanel epoch={run.runEpoch} active={center === 'history'} managementAvailable={run.historyManagementAvailable} onSettings={run.settingsAvailable ? () => { setDetails(false); setAccessOpen(false); setSettingsOpen(true); } : undefined} blocked={!!inspection || settingsOpen} onReturn={() => navigate('conversation')} onInspect={openCalls} onSelectionChange={() => setInspection(current => current?.source ? null : current)} />}
        {center === 'file' && file && <FileReading selection={file} onOpen={openFile} onClose={() => setCenter(priorCenter.current)} blocked={!!inspection || settingsOpen} />}
        {inspection && <CallInspector blocked={settingsOpen} reading={inspection.source?.reading || reading} epoch={inspection.source?.epoch || run.runEpoch} historical={!!inspection.source} before={inspection.source?.before} requestId={inspection.requestId} onSelect={requestId => setInspection({ ...inspection, requestId })} onClose={closeCalls} />}
        {run.settingsAvailable && <SettingsPanel key={run.runEpoch} epoch={run.runEpoch} open={settingsOpen} managementAvailable={run.historyManagementAvailable} onClose={closeSettings} onDirtyChange={setSettingsDirty} onViewHistory={() => { closeSettings(false); navigate('history'); }} />}
        {!following && center === 'conversation' && !inspection && <button className="wb-follow" onClick={() => { followRef.current = true; setFollowing(true); }}>↓ 跟随最新</button>}
      </main>
      <div className="wb-terminal-container" hidden={!terminal}>{run.terminalAvailable && <TerminalPanel epoch={run.runEpoch} />}</div>
    </div>
    {details && <section id="wb-run-overview" className="wb-status-details" aria-label="用量与状态" onKeyDown={event => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); closeDetails(); }
    }}>
      <div className="wb-panel-heading"><strong>用量概览</strong><button onClick={closeDetails} aria-label="关闭用量概览">×</button></div>
      <UsageOverview summary={reading.usageSummary} connected={reading.connected} />
      <button className="wb-view-calls" onClick={() => openCalls(null)}>查看调用记录 <span aria-hidden="true">→</span></button>
      <SaveOverview reading={reading} />
      <RunDiagnostics reading={reading} epoch={run.runEpoch} processId={run.processId} />
      {stopConfirm ? <div className="wb-confirm" role="alertdialog" aria-label="确认停止运行"><p>将停止当前原生 CLI 和正在执行的任务。内存阅读仍可查看。</p><button onClick={() => void stop()}>确认停止</button><button onClick={() => setStopConfirm(false)}>取消</button></div>
        : <button disabled={stopping} onClick={() => setStopConfirm(true)}>{stopping ? '已请求停止' : '停止当前运行'}</button>}
      {stopError && <p role="status">{stopError}</p>}
    </section>}
    <UsageFooter reading={reading} expanded={details} onToggle={() => { setInspection(null); setDetails(!details); }} />
  </div></WorkspaceLinks.Provider>;
}
