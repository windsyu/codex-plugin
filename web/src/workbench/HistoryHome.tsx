import { navigationURL } from './context';
import { workbenchFetch } from './context';
import './library.css';
import { useEffect, useRef, useState } from 'preact/hooks';
import { SettingsPanel } from './SettingsPanel';
import { RelayPanel } from './RelayPanel';
import { LaunchPanel } from './LaunchPanel';
import { useLaunch } from './useLaunch';
import { launchMessage } from './launchMessages';
import { HistoryLibraryPanel, type LibrarySource } from './HistoryLibraryPanel';
import { navigatePage, PageLink } from './navigation';
import { runPath, type HomePage } from './routes';
export interface ApplicationInfo {
  instanceId: string;
  runs: { runId: string; projectName: string; projectPath: string; state: string }[];
  sources: LibrarySource[];
  launchError: string | null;
  launchAvailable?: boolean;
  directoryPickerAvailable?: boolean;
  directoryBrowserAvailable?: boolean;
  remoteOwner?: boolean;
}
export function HistoryHome({ initial, page }: { initial: ApplicationInfo; page?: HomePage }) {
  const [info, setInfo] = useState(initial);
  const launch = useLaunch(initial.instanceId, id => { if (page) navigatePage(runPath(id)); else location.assign(navigationURL(runPath(id))); });
  const [error, setError] = useState('');
  const [open, setOpen] = useState(false);
  const [relayOpen, setRelayOpen] = useState(false);
  const stopping = useRef(new Set<string>());
  const [stoppingIds, setStoppingIds] = useState<string[]>([]);
  const [stopError, setStopError] = useState('');
  const stopRun = async (run: ApplicationInfo['runs'][number]) => {
    if (stopping.current.has(run.runId) || !window.confirm(`停止项目“${run.projectName}”的 Codex？该项目正在执行的任务将中止，其他项目继续运行。`)) return;
    stopping.current.add(run.runId); setStoppingIds([...stopping.current]); setStopError('');
    try {
      const response = await workbenchFetch(`/workbench/v1/runs/${encodeURIComponent(run.runId)}/stop`, { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ epoch: run.runId }) });
      if (!response.ok) throw new Error();
    } catch {
      stopping.current.delete(run.runId); setStoppingIds([...stopping.current]);
      setStopError(`未能确认项目“${run.projectName}”已停止，请检查该工作台状态。`);
    }
  };
  const [dirty, setDirty] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    document.title = '全部历史 · Codex 工作台';
    const controller = new AbortController();
    let timer: number;
    const update = async () => {
      try {
        const response = await workbenchFetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
        if (!response.ok) throw new Error('应用连接已断开，请重新打开配对入口。');
        const value: ApplicationInfo = await response.json();
        if (value.instanceId !== initial.instanceId) throw new Error('应用已重新启动，请重新打开首页。');
        if (!controller.signal.aborted) {
          for (const id of stopping.current) {
            const run = value.runs.find(item => item.runId === id);
            if (!run || (run.state !== 'running' && run.state !== 'stopping')) stopping.current.delete(id);
          }
          setStoppingIds([...stopping.current]); setInfo(value); setError('');
        }
      } catch (e) { if (!controller.signal.aborted) setError((e as Error).message); }
      finally { if (!controller.signal.aborted) timer = window.setTimeout(() => void update(), 2000); }
    };
    void update();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [initial.instanceId]);
  useEffect(() => { if (page) document.title = `${{ overview: '项目概览', history: '全部历史', launch: '启动项目', settings: '工作台设置', relay: '公网接入' }[page]} · Codex 工作台`; }, [page]);
  return <div className={`wb-home${page ? ' wb-pages' : ''}`}>
    {page ? <><header className="wb-header"><PageLink path="/" className="wb-wordmark"><span className="wb-brand">›_</span><strong>Codex <span>工作台</span></strong></PageLink><span className="wb-context-label">{info.remoteOwner ? '远程工作空间' : '本机工作空间'}</span>{info.remoteOwner && <a className="wb-portal-link" href="/">全部电脑 ↗</a>}</header><nav className="wb-page-nav" aria-label="应用导航"><PageLink path="/" active={page === 'overview'}>项目概览</PageLink><PageLink path="/history" active={page === 'history'}>全部历史</PageLink>{info.launchAvailable && <PageLink path="/launch" active={page === 'launch'}>启动项目</PageLink>}<PageLink path="/settings" active={page === 'settings'}>设置{dirty ? ' · 未保存' : ''}</PageLink>{!info.remoteOwner && <PageLink path="/relay" active={page === 'relay'}>公网接入</PageLink>}</nav></> : <header className="wb-header"><span className="wb-brand">›_</span><strong>Codex 工作台</strong><div className="wb-header-actions">{info.launchAvailable && <><button disabled={launch.picking} onClick={() => { if (launch.operationActive || !info.directoryPickerAvailable) launch.choose({ path: '' }, false); else void launch.pickDirectory(); }} aria-expanded={launch.open}>{launch.operationActive ? '启动进度' : launch.picking ? '正在选择文件夹…' : '打开其他目录'}</button>{info.directoryPickerAvailable && !launch.operationActive && <button disabled={launch.picking} onClick={() => launch.choose({ path: '' }, false)}>输入路径</button>}</>}{!info.remoteOwner && <button type="button" aria-expanded={relayOpen} onClick={() => { setOpen(false); setRelayOpen(!relayOpen); }}>公网接入</button>}<button id="wb-settings-button" ref={button} aria-expanded={open} aria-controls="wb-settings-panel" onClick={() => { setRelayOpen(false); setOpen(!open); }}>设置{dirty ? ' · 未保存' : ''}</button></div></header>}
    {error && <p className="wb-notice" role="alert">{error}</p>}
    {stopError && <p className="wb-notice" role="alert">{stopError}</p>}
    {info.launchError && <p className="wb-notice" role="alert">{launchMessage(info.launchError)}首页仍可使用。</p>}
    {page === 'overview' && <section className="wb-overview-heading"><div><span className="wb-eyebrow">YOUR WORKSPACE</span><h1>从这里，继续创作。</h1><p>打开项目，回到正在进行的工作。</p></div>{info.launchAvailable && <PageLink path="/launch" className="wb-primary-link">＋ 启动项目</PageLink>}<div className="wb-overview-stats"><span><strong>{info.runs.filter(run => run.state === 'running').length}</strong>运行中的项目</span><span><strong>{info.runs.length}</strong>本次运行</span><span><strong>{info.sources.length}</strong>历史来源</span></div></section>}
    {(!page || page === 'overview') && info.runs.length > 0 && <nav className="wb-home-runs" aria-label="项目工作台">
      <span className="wb-home-runs-label">工作台</span>
      <div className="wb-home-run-list">{info.runs.map(run => <div className="wb-home-run-card" key={run.runId}><a
        className={`wb-home-run-link${run.state === 'running' ? ' is-running' : ''}`}
        href={navigationURL(page ? runPath(run.runId) : `/?run=${encodeURIComponent(run.runId)}`)} target="_blank" rel="noopener"
        title={`${run.projectPath}\n${run.state === 'running' ? '进入工作台' : '查看已结束的运行'}（新标签页）`}
        aria-label={`${run.projectName} · ${run.state === 'running' ? '运行中，进入工作台' : '已结束，查看运行'}（新标签页）`}
      >
        <svg className="wb-home-run-terminal" width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><rect x="2" y="3" width="16" height="14" rx="2" /><path d="m5.5 7 3 3-3 3M11 13h3.5" /></svg>
        <span className="wb-home-run-name">{run.projectName}</span>
        <span className="wb-home-run-status"><span aria-hidden="true" />{stoppingIds.includes(run.runId) || run.state === 'stopping' ? '停止中…' : run.state === 'running' ? '运行中' : '已结束'}</span>
        <span className="wb-home-run-action"><span>{run.state === 'running' ? '进入工作台' : '查看运行'}</span><span aria-hidden="true">↗</span></span>
      </a>{run.state === 'running' && <button className="wb-home-run-stop" aria-label={`停止项目 ${run.projectName}`} disabled={stoppingIds.includes(run.runId)} onClick={() => void stopRun(run)}>{stoppingIds.includes(run.runId) ? '停止中…' : '停止'}</button>}</div>)}</div>
    </nav>}
    {page === 'overview' && !info.runs.length && <div className="wb-overview-empty"><h2>还没有打开的项目</h2><p>启动一个项目，即可连接原生 Codex 并阅读实时对话。</p><PageLink path="/history">浏览已保存的历史 →</PageLink></div>}
    {page === 'overview' && <section className="wb-overview-sources"><h2>历史来源</h2>{info.sources.map(source => <p key={source.id}><strong>{source.kind === 'native' ? '原生会话' : source.kind === 'workbench' ? '工作台记录' : '旧版历史'}</strong><span>{source.message || source.state}</span></p>)}</section>}
    {(page === 'launch' || (!page && launch.open)) && (info.launchAvailable ? <div className={page ? 'wb-document-page' : ''}>{page === 'launch' && <div className="wb-page-heading"><span className="wb-eyebrow">START</span><h1>启动项目</h1><p>选择一个项目目录，连接原生 Codex，实时阅读对话与执行细节。</p></div>}<LaunchPanel {...launch.panel} embedded={!!page} onClose={page ? () => navigatePage('/') : launch.panel.onClose} onPick={!info.remoteOwner && info.directoryPickerAvailable ? launch.panel.onPick : undefined} browserInstanceId={info.directoryBrowserAvailable && info.instanceId === initial.instanceId ? initial.instanceId : undefined} /></div> : <p className="wb-notice">当前连接不支持启动项目。</p>)}
    {(!page || page === 'history') && <HistoryLibraryPanel instanceId={page ? info.instanceId : undefined} sources={info.sources} onProjectLaunch={info.launchAvailable ? id => { launch.openProject(id); if (page) navigatePage('/launch'); } : undefined} onResume={info.launchAvailable ? entry => { launch.openResume(entry); if (page) navigatePage('/launch'); } : undefined} runs={info.runs} />}
    <div className={page ? 'wb-document-page' : ''} hidden={!!page && page !== 'settings'}><SettingsPanel epoch={info.instanceId} instanceId={info.instanceId} embedded={!!page} open={page ? page === 'settings' : open} onClose={restore => { if (page) navigatePage('/'); else { setOpen(false); if (restore) button.current?.focus(); } }} onDirtyChange={setDirty} /></div>
    {!info.remoteOwner && <div className={page ? 'wb-document-page' : ''} hidden={!!page && page !== 'relay'}><RelayPanel instanceId={info.instanceId} open={page ? page === 'relay' : relayOpen} onClose={() => page ? navigatePage('/') : setRelayOpen(false)} /></div>}
  </div>;
}
