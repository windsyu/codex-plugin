import './library.css';
import { useEffect, useRef, useState } from 'preact/hooks';
import { SettingsPanel } from './SettingsPanel';
import { LaunchPanel } from './LaunchPanel';
import { useLaunch } from './useLaunch';
import { launchMessage } from './launchMessages';
import { HistoryLibraryPanel, type LibrarySource } from './HistoryLibraryPanel';
export interface ApplicationInfo {
  instanceId: string;
  runs: { runId: string; projectName: string; projectPath: string; state: string }[];
  sources: LibrarySource[];
  launchError: string | null;
  launchAvailable?: boolean;
  directoryPickerAvailable?: boolean;
}
export function HistoryHome({ initial }: { initial: ApplicationInfo }) {
  const [info, setInfo] = useState(initial);
  const launch = useLaunch(initial.instanceId, id => { location.assign(`/?run=${encodeURIComponent(id)}`); });
  const [error, setError] = useState('');
  const [open, setOpen] = useState(false);
  const stopping = useRef(new Set<string>());
  const [stoppingIds, setStoppingIds] = useState<string[]>([]);
  const [stopError, setStopError] = useState('');
  const stopRun = async (run: ApplicationInfo['runs'][number]) => {
    if (stopping.current.has(run.runId) || !window.confirm(`停止项目“${run.projectName}”的 Codex？该项目正在执行的任务将中止，其他项目继续运行。`)) return;
    stopping.current.add(run.runId); setStoppingIds([...stopping.current]); setStopError('');
    try {
      const response = await fetch(`/workbench/v1/runs/${encodeURIComponent(run.runId)}/stop`, { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ epoch: run.runId }) });
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
        const response = await fetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
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
  return <div className="wb-home">
    <header className="wb-header"><span className="wb-brand">›_</span><strong>Codex 工作台</strong><div className="wb-header-actions">{info.launchAvailable && <><button disabled={launch.picking} onClick={() => { if (launch.operationActive || !info.directoryPickerAvailable) launch.choose({ path: '' }, false); else void launch.pickDirectory(); }} aria-expanded={launch.open}>{launch.operationActive ? '启动进度' : launch.picking ? '正在选择文件夹…' : '打开其他目录'}</button>{info.directoryPickerAvailable && !launch.operationActive && <button disabled={launch.picking} onClick={() => launch.choose({ path: '' }, false)}>输入路径</button>}</>}<button id="wb-settings-button" ref={button} aria-expanded={open} aria-controls="wb-settings-panel" onClick={() => setOpen(!open)}>设置{dirty ? ' · 未保存' : ''}</button></div></header>
    {error && <p className="wb-notice" role="alert">{error}</p>}
    {stopError && <p className="wb-notice" role="alert">{stopError}</p>}
    {info.launchError && <p className="wb-notice" role="alert">{launchMessage(info.launchError)}首页仍可使用。</p>}
    {info.runs.length > 0 && <nav className="wb-home-runs" aria-label="项目工作台">
      <span className="wb-home-runs-label">工作台</span>
      <div className="wb-home-run-list">{info.runs.map(run => <div className="wb-home-run-card" key={run.runId}><a
        className={`wb-home-run-link${run.state === 'running' ? ' is-running' : ''}`}
        href={`/?run=${encodeURIComponent(run.runId)}`} target="_blank" rel="noopener"
        title={`${run.projectPath}\n${run.state === 'running' ? '进入工作台' : '查看已结束的运行'}（新标签页）`}
        aria-label={`${run.projectName} · ${run.state === 'running' ? '运行中，进入工作台' : '已结束，查看运行'}（新标签页）`}
      >
        <svg className="wb-home-run-terminal" width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><rect x="2" y="3" width="16" height="14" rx="2" /><path d="m5.5 7 3 3-3 3M11 13h3.5" /></svg>
        <span className="wb-home-run-name">{run.projectName}</span>
        <span className="wb-home-run-status"><span aria-hidden="true" />{stoppingIds.includes(run.runId) || run.state === 'stopping' ? '停止中…' : run.state === 'running' ? '运行中' : '已结束'}</span>
        <span className="wb-home-run-action"><span>{run.state === 'running' ? '进入工作台' : '查看运行'}</span><span aria-hidden="true">↗</span></span>
      </a>{run.state === 'running' && <button className="wb-home-run-stop" aria-label={`停止项目 ${run.projectName}`} disabled={stoppingIds.includes(run.runId)} onClick={() => void stopRun(run)}>{stoppingIds.includes(run.runId) ? '停止中…' : '停止'}</button>}</div>)}</div>
    </nav>}
    {launch.open && <LaunchPanel {...launch.panel} onPick={info.directoryPickerAvailable ? launch.panel.onPick : undefined} />}
    <HistoryLibraryPanel sources={info.sources} onProjectLaunch={info.launchAvailable ? launch.openProject : undefined} onResume={info.launchAvailable ? launch.openResume : undefined} runs={info.runs} />
    <SettingsPanel epoch={info.instanceId} instanceId={info.instanceId} open={open} onClose={restore => { setOpen(false); if (restore) button.current?.focus(); }} onDirtyChange={setDirty} />
  </div>;
}
