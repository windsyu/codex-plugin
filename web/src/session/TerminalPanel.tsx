import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import '@xterm/xterm/css/xterm.css';

import type { SessionAttachment, SessionWorkerSnapshot, TerminalSnapshotFrame } from '../types';
import {
  decodeOutputFrame,
  decodeSnapshot,
  encodeClientFrame,
  terminalProtocols,
  terminalWebSocketUrl
} from './terminalProtocol';
import { TerminalResizeScheduler } from './terminalResize';
import { CODEX_TERMINAL_THEME } from './terminalTheme';
import { TerminalWriteCoordinator } from './terminalWriteCoordinator';

type SocketState = 'connecting' | 'live' | 'disconnected';

interface TerminalPanelProps {
  workerId: string;
  attachment: SessionAttachment;
  leaseId?: string;
  onConnected: () => Promise<string | undefined>;
  onDisconnected: () => void;
  onWorker: (worker: SessionWorkerSnapshot) => void;
  onSnapshot: (snapshot: TerminalSnapshotFrame) => void;
  onError: (message: string) => void;
}

type TerminalServerFrame = TerminalSnapshotFrame
  | { type: 'state'; worker: SessionWorkerSnapshot }
  | { type: 'error'; code: string; message: string };

export function TerminalPanel({ workerId, attachment, leaseId, onConnected, onDisconnected, onWorker, onSnapshot, onError }: TerminalPanelProps) {
  const host = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal>();
  const socketRef = useRef<WebSocket>();
  const leaseRef = useRef(leaseId);
  const resizeSchedulerRef = useRef<TerminalResizeScheduler>();
  const [socketState, setSocketState] = useState<SocketState>('connecting');
  const [terminalScrolled, setTerminalScrolled] = useState(false);

  function focusTerminalInput() {
    const terminal = terminalRef.current;
    if (!terminal || !leaseRef.current) return;
    terminal.scrollToBottom();
    terminal.focus();
    setTerminalScrolled(false);
  }

  useLayoutEffect(() => {
    leaseRef.current = leaseId;
    if (terminalRef.current) terminalRef.current.options.disableStdin = !leaseId;
    resizeSchedulerRef.current?.setEnabled(Boolean(leaseId && socketRef.current?.readyState === WebSocket.OPEN));
  }, [leaseId]);

  useLayoutEffect(() => {
    if (!host.current) return;
    let disposed = false;
    let fitFrame: number | undefined;
    let observedWidth = -1;
    let observedHeight = -1;
    let lastOutputSeq = 0;
    const terminal = new Terminal({
      // Unicode version selection is still exposed through xterm's proposed API.
      // It is enabled only for the bundled Unicode 11 addon below.
      allowProposedApi: true,
      convertEol: false,
      cursorBlink: true,
      disableStdin: !leaseRef.current,
      drawBoldTextInBrightColors: true,
      fontFamily: '"SF Mono", Menlo, Monaco, Consolas, "Cascadia Mono", "PingFang SC", "Microsoft YaHei", monospace',
      fontSize: 14,
      fontWeight: '400',
      fontWeightBold: '700',
      letterSpacing: 0,
      lineHeight: 1.2,
      minimumContrastRatio: 1,
      scrollOnUserInput: true,
      scrollback: 5_000,
      smoothScrollDuration: 0,
      theme: CODEX_TERMINAL_THEME
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    const unicode11 = new Unicode11Addon();
    terminal.loadAddon(unicode11);
    terminal.unicode.activeVersion = '11';
    terminal.open(host.current);
    terminalRef.current = terminal;
    const writer = new TerminalWriteCoordinator(terminal);
    const viewport = host.current.querySelector<HTMLElement>('.xterm-viewport');
    const socket = new WebSocket(terminalWebSocketUrl(workerId), terminalProtocols(attachment.descriptor));
    socket.binaryType = 'arraybuffer';
    socketRef.current = socket;
    const send = (value: object) => {
      if (socket.readyState !== WebSocket.OPEN) return false;
      socket.send(encodeClientFrame(value as never));
      return true;
    };
    const updateScrollState = () => {
      if (disposed) return;
      const viewportIsAboveBottom = viewport
        ? viewport.scrollTop + viewport.clientHeight < viewport.scrollHeight - 1
        : terminal.buffer.active.viewportY < terminal.buffer.active.baseY;
      setTerminalScrolled(viewportIsAboveBottom);
    };
    const resizeScheduler = new TerminalResizeScheduler(
      () => ({ cols:terminal.cols, rows:terminal.rows }),
      ({ cols, rows }) => send({ type:'resize', cols, rows })
    );
    resizeSchedulerRef.current = resizeScheduler;
    const scheduleFit = () => {
      if (disposed || fitFrame !== undefined) return;
      fitFrame = window.requestAnimationFrame(() => {
        fitFrame = undefined;
        try { fit.fit(); } catch { /* xterm may be between layout and disposal */ }
      });
    };
    const input = terminal.onData((data) => {
      const activeLease = leaseRef.current;
      if (activeLease) {
        terminal.scrollToBottom();
        setTerminalScrolled(false);
        send({ type: 'input', leaseId: activeLease, data });
      }
    });
    const scroll = terminal.onScroll((viewportY) => {
      if (!disposed) setTerminalScrolled(viewportY < terminal.buffer.active.baseY);
    });
    viewport?.addEventListener('scroll', updateScrollState, { passive: true });
    const resize = terminal.onResize(() => resizeScheduler.schedule());
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const width = Math.round(entry.contentRect.width);
      const height = Math.round(entry.contentRect.height);
      if (width === observedWidth && height === observedHeight) return;
      observedWidth = width;
      observedHeight = height;
      scheduleFit();
    });
    observer.observe(host.current);
    void document.fonts?.ready.then(() => {
      if (disposed) return;
      scheduleFit();
      terminal.refresh(0, Math.max(0, terminal.rows - 1));
    });
    socket.onopen = () => {
      setSocketState('live');
      scheduleFit();
      void onConnected().then((acquiredLeaseId) => {
        if (disposed || !acquiredLeaseId) return;
        leaseRef.current = acquiredLeaseId;
        terminal.options.disableStdin = false;
        resizeScheduler.setEnabled(true);
        terminal.scrollToBottom();
        setTerminalScrolled(false);
        terminal.focus();
      });
    };
    socket.onmessage = async (event) => {
      try {
        if (event.data instanceof ArrayBuffer) {
          const frame = decodeOutputFrame(event.data);
          if (frame.outputSeq <= lastOutputSeq) return;
          lastOutputSeq = frame.outputSeq;
          const wasAtBottom = terminal.buffer.active.viewportY >= terminal.buffer.active.baseY;
          writer.write(frame.data, () => {
            if (wasAtBottom) terminal.scrollToBottom();
            updateScrollState();
            send({ type: 'ack', outputSeq: frame.outputSeq });
          });
          return;
        }
        const frame = JSON.parse(String(event.data)) as TerminalServerFrame;
        if (frame.type === 'snapshot') {
          const restored = decodeSnapshot(frame);
          lastOutputSeq = frame.toSeq;
          onSnapshot(frame);
          writer.restore(
            restored.screen,
            restored.replay,
            () => terminal.resize(frame.cols, frame.rows),
            () => {
              terminal.scrollToBottom();
              setTerminalScrolled(false);
              send({ type: 'ack', outputSeq: frame.toSeq });
            }
          );
        } else if (frame.type === 'state') {
          onWorker(frame.worker);
        } else if (frame.type === 'error') {
          onError(`${frame.code}：${frame.message}`);
        }
      } catch (error) {
        onError(error instanceof Error ? error.message : '终端 frame 无法解析');
        socket.close(1002, 'invalid terminal frame');
      }
    };
    socket.onerror = () => onError('终端 WebSocket 连接失败');
    socket.onclose = () => {
      setSocketState('disconnected');
      resizeScheduler.setEnabled(false);
      if (!disposed) onDisconnected();
    };

    return () => {
      disposed = true;
      if (fitFrame !== undefined) window.cancelAnimationFrame(fitFrame);
      resizeScheduler.dispose();
      resizeSchedulerRef.current = undefined;
      observer.disconnect();
      input.dispose();
      scroll.dispose();
      viewport?.removeEventListener('scroll', updateScrollState);
      resize.dispose();
      socket.close(1000, 'terminal detached');
      writer.dispose();
      terminal.dispose();
      terminalRef.current = undefined;
      socketRef.current = undefined;
    };
  }, [workerId, attachment.descriptor]);

  return <div class="terminal-panel-wrap">
    <div class="terminal-titlebar">
      <span class="terminal-title"><b aria-hidden="true">&gt;_</b> Codex CLI</span>
      <div class="terminal-runtime-status">
        {leaseId ? <button type="button" class={`terminal-input-status terminal-input-status-active ${terminalScrolled ? 'terminal-input-status-attention' : ''}`}
          aria-label={terminalScrolled ? '回到底部并输入' : '定位到输入框'} onClick={focusTerminalInput}>
          <i aria-hidden="true" />{terminalScrolled ? '输入框在下方 ↓' : '输入已就绪'}
        </button> : <span class="terminal-input-status terminal-input-status-readonly" role="status"><i aria-hidden="true" />实时只读</span>}
        <div class={`terminal-transport terminal-transport-${socketState}`} role="status">
          <span />{socketState === 'live' ? 'xterm 已连接' : socketState === 'connecting' ? 'xterm 正在连接' : 'xterm 已断开'}
        </div>
      </div>
    </div>
    <div class="terminal-stage">
      <div ref={host} class="terminal-panel" aria-label="Codex PTY terminal" />
    </div>
  </div>;
}
