import { render } from 'preact';
import { App, type Run } from './App';
import './style.css';

async function start() {
  const pair = new URLSearchParams(location.hash.slice(1)).get('pair');
  history.replaceState(null, '', location.pathname);
  if (pair) {
    const response = await fetch('/workbench/v1/pair', {
      method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ token: pair })
    });
    if (!response.ok) throw new Error(response.status === 410 ? '配对链接已失效，请在电脑的“手机接入”中查看本次启动的二维码。' : response.status === 429 ? '配对尝试过于频繁，请稍后重试。' : response.status === 409 ? '已达到 8 个配对浏览器，请在电脑的“手机接入”中断开不再使用的浏览器。' : '配对未成功，请检查连接，或在电脑上查看本次启动的配对链接。');
  }
  const response = await fetch('/workbench/v1/run', { credentials: 'same-origin', cache: 'no-store' });
  if (!response.ok) throw new Error('请从本次运行的配对入口打开工作台。');
  const run: Run = await response.json();
  render(<App run={run} />, document.getElementById('workbench-root')!);
}
void start().catch(error => {
  render(<main className="wb-unavailable"><h1>Codex 工作台</h1><p>{error instanceof Error ? error.message : '连接失败，请重新打开本次运行入口。'}</p></main>, document.getElementById('workbench-root')!);
});
