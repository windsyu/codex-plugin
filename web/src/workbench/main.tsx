import { workbenchFetch } from './context';
import { render } from 'preact';
import { runApi } from './runApi';
import type { Run } from './App';
import './style.css';
import { HistoryHome, type ApplicationInfo } from './HistoryHome';
import { AUTH_INVALID, workbenchContext } from './context';
import { useEffect, useState } from 'preact/hooks';
import { App } from './App';
import { parseRoute, runPath } from './routes';
import { navigatePage, useRoute } from './navigation';
import './pages.css';

function Router({ application, initialRun }: { application: ApplicationInfo | null; initialRun?: Run }) {
  const route = useRoute();
  const selected = route.kind === 'run' ? route.runId : !application && route.kind === 'home' && route.page === 'overview' ? initialRun?.runEpoch : undefined;
  const [run, setRun] = useState(initialRun);
  const [error, setError] = useState('');
  useEffect(() => {
    setError('');
    if (!selected || selected === run?.runEpoch) return;
    const controller = new AbortController();
    void workbenchFetch(runApi('/run'), { credentials: 'same-origin', cache: 'no-store', signal: controller.signal }).then(async response => {
      if (!response.ok) throw new Error('这次运行不可用，请返回项目概览。');
      const value: Run = await response.json();
      if (value.runEpoch !== selected) throw new Error('工作台身份不匹配。');
      if (!controller.signal.aborted) setRun(value);
    }).catch(error => { if (!controller.signal.aborted) setError(error.message); });
    return () => controller.abort();
  }, [selected]);
  if (route.kind === 'missing') return <main className="wb-unavailable"><h1>页面不存在</h1><p>此工作台地址无效。</p></main>;
  if (route.kind === 'home' && application) return <HistoryHome initial={application} page={route.page} />;
  if (error) return <main className="wb-unavailable"><h1>无法打开运行</h1><p role="alert">{error}</p></main>;
  if (!run || run.runEpoch !== selected) return <main className="wb-unavailable" role="status">正在打开工作台…</main>;
  return <App key={run.runEpoch} run={run} applicationHome={!!application} page={route.kind === 'run' ? route.page : undefined} />;
}

function unavailable(message: string) {
  render(<main className="wb-unavailable"><h1>Codex 工作台</h1><p>{message}</p>{location.pathname.startsWith('/g/') && <a href="/">返回门户重新登录</a>}</main>, document.getElementById('workbench-root')!);
}
window.addEventListener(AUTH_INVALID, () => unavailable('登录或工作台授权已失效，请返回门户重新登录并打开工作台。'));

async function start() {
  const context = workbenchContext();
  const pair = new URLSearchParams(location.hash.slice(1)).get('pair');
  history.replaceState(history.state, '', location.pathname + location.search);
  context.assert();
  const route = parseRoute(new URL(location.href));
  if (route.kind === 'missing') throw new Error('页面不存在，请检查工作台地址。');
  if (pair) {
    const response = await workbenchFetch(runApi('/pair'), {
      method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: pair })
    });
    if (!response.ok) throw new Error(response.status === 410 ? '配对链接已失效，请在电脑的“本地局域网接入”中查看本次启动的二维码。' : response.status === 429 ? '配对尝试过于频繁，请稍后重试。' : response.status === 409 ? '已达到 8 个配对浏览器，请在电脑的“本地局域网接入”中断开不再使用的浏览器。' : '配对未成功，请检查连接，或在电脑上查看本次启动的配对链接。');
  }
  const applicationResponse = await workbenchFetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store' });
  let application: ApplicationInfo | null = null;
  if (applicationResponse.ok) {
    application = await applicationResponse.json();
    const runId = route.kind === 'run' ? route.runId : null;
    if (!runId) {
      context.assert(); render(<Router application={application} />, document.getElementById('workbench-root')!);
      return;
    }
    if (!application?.runs.some(run => run.runId === runId)) throw new Error('这次运行已结束或不属于当前应用，请返回首页。');
  } else if (applicationResponse.status !== 404) throw new Error('请从本次应用的配对入口打开页面。');
  if (!application && route.kind === 'home' && route.page !== 'overview') throw new Error('当前运行未提供应用页面，请返回本次运行入口。');
  const response = await workbenchFetch(runApi('/run'), { credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) throw new Error('请从本次运行的配对入口打开工作台。');
  const run: Run = await response.json();
  const requestedRun = route.kind === 'run' ? route.runId : null;
  if (requestedRun && run.runEpoch !== requestedRun) throw new Error('工作台身份不匹配，请返回首页重新进入。');
  if (requestedRun && !application) run.settingsAvailable = false;
  if (route.kind === 'run' && route.legacy) navigatePage(runPath(route.runId), true);
  context.assert(); render(<Router application={application} initialRun={run} />, document.getElementById('workbench-root')!);
}
void start().catch(error => {
  unavailable(error instanceof Error ? error.message : '连接失败，请重新打开本次运行入口。');
});
