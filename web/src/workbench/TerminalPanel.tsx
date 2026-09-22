import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import '@xterm/xterm/css/xterm.css';
import { CODEX_TERMINAL_THEME } from '../terminal/terminalTheme';
import { TerminalClient, initialTerminalView } from './terminalClient';

export function TerminalPanel({ epoch }: { epoch: string }) {
  const host = useRef<HTMLDivElement>(null);
  const client = useRef<TerminalClient>();
  const [view, setView] = useState(initialTerminalView);
  useLayoutEffect(() => {
    if (!host.current) return;
    const terminal = new Terminal({
      allowProposedApi: true, disableStdin: true, convertEol: false, cursorBlink: true,
      fontFamily: '"SF Mono", Menlo, "PingFang SC", monospace', fontSize: 13, lineHeight: 1.18,
      scrollback: 3000, theme: CODEX_TERMINAL_THEME, minimumContrastRatio: 1
    });
    const fit = new FitAddon(); terminal.loadAddon(fit);
    terminal.loadAddon(new Unicode11Addon()); terminal.unicode.activeVersion = '11';
    terminal.open(host.current);
    const connection = new TerminalClient(terminal, fit, epoch, setView); client.current = connection;
    const data = terminal.onData(text => connection.input(text));
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame); frame = requestAnimationFrame(() => connection.resize());
    });
    observer.observe(host.current);
    void document.fonts?.ready.then(() => connection.resize());
    return () => { cancelAnimationFrame(frame); observer.disconnect(); data.dispose(); connection.dispose(); terminal.dispose(); };
  }, [epoch]);
  const ended = view.control?.ended;
  const otherPage = view.ready && !view.owned && (view.control?.controllerConnection || view.control?.reconnectReserved);
  return <section className="wb-terminal" aria-label="原生 Codex 终端" data-owned={view.owned} data-ready={view.ready}>
    <div className="wb-panel-heading"><span>›_ 原生终端</span><span className="wb-terminal-status">{ended ? '已结束' : view.owned ? '已连接' : otherPage ? '只读' : '连接中'}</span></div>
    {!ended && otherPage && <p className="wb-notice">另一页面正在使用此终端。切换后，另一页面将暂停输入。 <button onClick={() => client.current?.takeover()}>在此输入</button></p>}
    {view.issue && <p className="wb-notice" role="status">{view.issue}</p>}
    {view.inputUncertain && <p className="wb-notice" role="status">部分输入的送达情况未确认；请检查终端，输入不会自动重发。 <button onClick={() => client.current?.acknowledgeInputUncertainty()}>已检查终端</button></p>}
    {view.screenUnavailable && <p className="wb-notice" role="status">屏幕恢复不可用，原生 CLI 与实时输出仍继续；刷新后只能显示后续输出。</p>}
    <div className="wb-xterm" ref={host} />
    <div className="wb-terminal-foot">{view.exit ? `CLI 已退出 · ${view.exit.signal || `退出码 ${view.exit.code}`}` : view.screenUnavailable ? '屏幕恢复异常 · 历史画面缺失' : view.partial ? '已恢复当前屏幕 · 更早终端输出可能已省略' : '当前运行的原生终端'}</div>
  </section>;
}
