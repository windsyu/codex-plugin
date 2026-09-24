import { render } from 'preact';
import { runApi } from './runApi';
import type { Run } from './App';
import './style.css';
import { HistoryHome, type ApplicationInfo } from './HistoryHome';

async function start() {
  const pair = new URLSearchParams(location.hash.slice(1)).get('pair');
  history.replaceState(history.state, '', location.pathname + location.search);
  if (pair) {
    const response = await fetch(runApi('/pair'), {
      method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: pair })
    });
    if (!response.ok) throw new Error(response.status === 410 ? '配对链接已失效，请在电脑的“手机接入”中查看本次启动的二维码。' : response.status === 429 ? '配对尝试过于频繁，请稍后重试。' : response.status === 409 ? '已达到 8 个配对浏览器，请在电脑的“手机接入”中断开不再使用的浏览器。' : '配对未成功，请检查连接，或在电脑上查看本次启动的配对链接。');
  }
  const applicationResponse = await fetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store' });
  let application: ApplicationInfo | null = null;
  if (applicationResponse.ok) {
    application = await applicationResponse.json();
    const runId = new URLSearchParams(location.search).get('run');
    if (!runId) {
      render(<HistoryHome initial={application!} />, document.getElementById('workbench-root')!);
      return;
    }
    if (!application?.runs.some(run => run.runId === runId)) throw new Error('这次运行已结束或不属于当前应用，请返回首页。');
  } else if (applicationResponse.status !== 404) throw new Error('请从本次应用的配对入口打开页面。');
  const response = await fetch(runApi('/run'), { credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) throw new Error('请从本次运行的配对入口打开工作台。');
  const run: Run = await response.json();
  const requestedRun = new URLSearchParams(location.search).get('run');
  if (requestedRun && run.runEpoch !== requestedRun) throw new Error('工作台身份不匹配，请返回首页重新进入。');
  if (requestedRun && !application) run.settingsAvailable = false;
  const { App } = await import('./App');
  render(<App run={run} applicationHome={!!application} />, document.getElementById('workbench-root')!);
}
void start().catch(error => {
  render(<main className="wb-unavailable"><h1>Codex 工作台</h1><p>{error instanceof Error ? error.message : '连接失败，请重新打开本次运行入口。'}</p></main>, document.getElementById('workbench-root')!);
});
