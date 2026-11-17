import type { ComponentChildren } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';
import type { ApplicationInfo } from './HistoryHome';
import type { Run } from './App';
import { workbenchContext, workbenchFetch } from './context';
import { PageLink } from './navigation';
import { runPath, type WorkbenchRoute } from './routes';
import { Icon } from './Icons';
import './frame.css';

export function useApplicationStatus(initial: ApplicationInfo | null) {
  const [result, setResult] = useState({ info: initial, error: '' });
  useEffect(() => {
    if (!initial) return;
    const controller = new AbortController();
    let timer = 0;
    const update = async () => {
      try {
        const response = await workbenchFetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
        if (!response.ok) throw new Error('应用连接已断开，已有内容仍可阅读。');
        const info: ApplicationInfo = await response.json();
        if (info.instanceId !== initial.instanceId) throw new Error('应用已重新启动，请重新打开工作台。');
        if (!controller.signal.aborted) setResult({ info, error: '' });
      } catch (error) {
        if (!controller.signal.aborted) setResult(current => ({ ...current, error: (error as Error).message === '应用已重新启动，请重新打开工作台。' ? '应用已重新启动，请重新打开工作台。' : '应用连接已断开，已有内容仍可阅读。' }));
      } finally {
        if (!controller.signal.aborted) timer = window.setTimeout(() => void update(), 2000);
      }
    };
    // The bootstrap response already supplies the first complete application view.
    timer = window.setTimeout(() => void update(), 2000);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [initial?.instanceId]);
  return result;
}

function useComputerIdentity(application: ApplicationInfo | null) {
  const context = workbenchContext();
  const [identity, setIdentity] = useState<{ name?: string; portal?: string; state?: string }>({});
  useEffect(() => {
    if (!application || (application.remoteOwner && !context.gateway)) return;
    const controller = new AbortController();
    let timer = 0;
    const read = async () => {
      try {
        if (context.gateway) {
          // Display metadata comes from the portal; every business request keeps
          // using the original, fixed computer grant in workbenchContext.
          const response = await fetch('/relay/v1/gateways', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
          if (!response.ok) return;
          const value = await response.json();
          context.assert();
          const computer = Array.isArray(value.gateways) ? value.gateways.find((item: { gatewayId?: string }) => item.gatewayId === context.gateway) : null;
          if (!controller.signal.aborted && typeof computer?.name === 'string') setIdentity({ name: computer.name });
        } else {
          const response = await workbenchFetch('/workbench/v1/application/relay', { credentials: 'same-origin', cache: 'no-store', signal: controller.signal });
          if (!response.ok) return;
          const value = await response.json();
          if (value.instanceId !== application.instanceId || !value.relay) return;
          let portal: string | undefined;
          try {
            const url = new URL(value.relay.serverUrl);
            if (url.protocol === 'https:' && !url.username && !url.password && url.pathname === '/' && !url.search && !url.hash) portal = url.origin + '/computers';
          } catch { /* Unconfigured or invalid metadata never becomes a link. */ }
          if (!controller.signal.aborted) setIdentity({ name: typeof value.relay.name === 'string' ? value.relay.name : undefined, state: value.relay.state, portal });
        }
      } catch { /* Identity metadata is optional and cannot interrupt the workspace. */ }
      finally { if (!controller.signal.aborted && !context.gateway) timer = window.setTimeout(() => void read(), 5000); }
    };
    void read();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [application?.instanceId, application?.remoteOwner, context.gateway]);
  const remote = !!context.gateway || !!application?.remoteOwner;
  return {
    name: identity.name || (remote ? `电脑 ${context.gateway?.slice(0, 8) || '身份待确认'}` : application ? '此电脑' : '当前运行'),
    description: remote ? '远程工作台' : application ? '本机工作台' : '仅当前运行',
    portal: remote ? '/computers' : identity.portal,
    remote,
    connector: ({ online: '公网已开启', connecting: '公网连接中', offline: '公网离线', authenticationRequired: '需重新登录', disabled: '公网已关闭' } as Record<string, string>)[identity.state || ''] || '连接管理'
  };
}

export function WorkbenchFrame({ application, route, run, error, children }: {
  application: ApplicationInfo | null; route: WorkbenchRoute; run?: Run; error?: string; children: ComponentChildren;
}) {
  const identity = useComputerIdentity(application);
  const [navigationOpen, setNavigationOpen] = useState(false);
  const [query, setQuery] = useState('');
  const menu = useRef<HTMLButtonElement>(null);
  const drawer = useRef<HTMLDialogElement>(null);
  const sidebar = useRef<HTMLElement>(null);
  const slot = useRef<HTMLDivElement>(null);
  const desktop = useRef<HTMLDivElement>(null);
  const activeRun = route.kind === 'run' ? route.runId : undefined;
  const workspacePath = run && (!application || application.runs.some(item => item.runId === run.runEpoch)) ? runPath(run.runEpoch) : '/';
  useEffect(() => {
    if (!navigationOpen || !sidebar.current || !slot.current || !drawer.current) return;
    const element = sidebar.current;
    slot.current.append(element);
    drawer.current.showModal();
    const closeOnResize = () => { if (window.innerWidth >= 768) setNavigationOpen(false); };
    window.addEventListener('resize', closeOnResize);
    return () => {
      window.removeEventListener('resize', closeOnResize);
      drawer.current?.close();
      desktop.current?.append(element);
      menu.current?.focus({ preventScroll: true });
    };
  }, [navigationOpen]);
  useEffect(() => { setNavigationOpen(false); }, [route.kind, route.kind === 'run' ? route.runId + route.page : route.kind === 'home' ? route.page : '']);
  const runs = application?.runs.filter(item => `${item.projectName} ${item.projectPath}`.toLocaleLowerCase().includes(query.toLocaleLowerCase())) || [];
  const navigation = <aside className="wb-project-navigation" ref={sidebar} aria-label="项目与会话">
    <div className="wb-navigation-heading"><span>项目与会话</span><button className="wb-mobile-only" aria-label="收起项目与会话" onClick={() => setNavigationOpen(false)}><Icon name="close" /></button></div>
    {application?.launchAvailable && <PageLink className="wb-new-project" path="/launch"><span aria-hidden="true">＋</span> 新建会话</PageLink>}
    {application && <><label className="wb-navigation-search"><Icon name="search" /><input type="search" aria-label="查找打开的项目" placeholder="查找项目" value={query} onInput={event => setQuery(event.currentTarget.value)} /></label>
      <div className="wb-navigation-section"><span>打开的项目</span><span>{application.runs.length}</span></div>
      <div className="wb-navigation-projects">{runs.map(item => <div className="wb-navigation-project" key={item.runId}>
        <PageLink path={runPath(item.runId)} active={activeRun === item.runId} className="wb-project-entry"><Icon name="folder" /><span><strong>{item.projectName}</strong><small title={item.projectPath}>{item.projectPath}</small></span></PageLink>
        <PageLink path={runPath(item.runId)} active={activeRun === item.runId && route.kind === 'run' && route.page === 'chat'} className="wb-session-entry"><span className={`wb-state-dot ${item.state === 'running' ? 'is-live' : item.state === 'stopping' ? 'is-waiting' : ''}`} /><span>当前会话</span><small>{item.state === 'running' ? '运行中' : item.state === 'stopping' ? '停止中' : '已结束'}</small></PageLink>
      </div>)}{!runs.length && <p className="wb-navigation-empty">{query ? '没有匹配的项目' : '打开一个项目，开始新的会话。'}</p>}</div>
      <PageLink className="wb-navigation-history" path="/history" active={route.kind === 'home' && route.page === 'history'}><Icon name="history" /> 全部历史 <Icon name="chevron" /></PageLink>
    </>}
    {!application && run && <><div className="wb-navigation-section">当前项目</div><div className="wb-navigation-project"><PageLink className="wb-project-entry" path={runPath(run.runEpoch)}><Icon name="folder" /><strong>{run.projectName || '当前项目'}</strong></PageLink><p className="wb-navigation-empty">此入口仅访问当前运行，其他项目和全局历史不在此授权范围内。</p></div></>}
    <div className="wb-navigation-utilities">
      {run && route.kind === 'run' && <>{run.historyAvailable && <PageLink path={runPath(run.runEpoch, 'history')} active={route.page === 'history'}><Icon name="history" /> 项目运行历史</PageLink>}<PageLink path={runPath(run.runEpoch, 'usage')} active={route.page === 'usage'}><Icon name="usage" /> 用量与状态</PageLink>{run.accessAvailable && <PageLink path={runPath(run.runEpoch, 'access')} active={route.page === 'access'}><Icon name="devices" /> 本地局域网</PageLink>}{!application && run.settingsAvailable && <PageLink path={runPath(run.runEpoch, 'settings')} active={route.page === 'settings'}><Icon name="settings" /> 设置</PageLink>}</>}
      {application && <><PageLink path="/settings" active={route.kind === 'home' && route.page === 'settings'}><Icon name="settings" /> 设置</PageLink>{!identity.remote && <PageLink path="/relay" active={route.kind === 'home' && route.page === 'relay'}><Icon name="globe" /> 远程连接 <small>{identity.connector}</small></PageLink>}</>}
      <div className="wb-navigation-identity"><Icon name="monitor" /><span><strong>{identity.name}</strong><small>{identity.description}</small></span></div>
    </div>
  </aside>;
  return <div className={`wb-product ${identity.remote ? 'is-remote' : ''}`}>
    <header className="wb-product-header">
      <button ref={menu} className="wb-mobile-only wb-navigation-toggle" aria-label="展开项目与会话" aria-expanded={navigationOpen} onClick={() => setNavigationOpen(true)}><Icon name="menu" /></button>
      <PageLink path={workspacePath} className="wb-product-brand"><span>›_</span><strong>Codex <em>View</em></strong></PageLink>
      <nav className="wb-product-nav" aria-label="产品导航"><PageLink path={workspacePath} active={route.kind !== 'home' || route.page !== 'relay'}><Icon name="chat" /> 工作台</PageLink>{identity.portal && <a href={identity.portal} {...(!identity.remote ? { target: '_blank', rel: 'noopener noreferrer' } : {})}><Icon name="monitor" /> 我的电脑</a>}{application && !identity.remote && <PageLink path="/relay" active={route.kind === 'home' && route.page === 'relay'}><Icon name="globe" /> 连接管理</PageLink>}{run?.accessAvailable && identity.remote && <PageLink path={runPath(run.runEpoch, 'access')} active={route.kind === 'run' && route.page === 'access'}><Icon name="devices" /> 连接管理</PageLink>}</nav>
      <div className="wb-product-identity"><Icon name="monitor" /><span><strong>{identity.name}</strong><small>{identity.description}</small></span></div>
    </header>
    <div className="wb-product-body"><div className="wb-navigation-desktop" ref={desktop}>{navigation}</div><div className="wb-product-content">{error && <p className="wb-product-notice" role="status">{error}</p>}{children}</div></div>
    <dialog className="wb-navigation-dialog" ref={drawer} aria-label="项目与会话导航" onCancel={event => { event.preventDefault(); setNavigationOpen(false); }} onClick={event => { if (event.target === drawer.current) setNavigationOpen(false); }}><div ref={slot} /></dialog>
  </div>;
}
