import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

import type { SessionAttachment, SessionWorker, TerminalSnapshotFrame } from '../types';
import {
  decodeOutputFrame,
  decodeSnapshot,
  encodeClientFrame,
  terminalProtocols,
  terminalWebSocketUrl
} from './terminalProtocol';

type SocketState = 'connecting' | 'live' | 'disconnected';

interface TerminalPanelProps {
  workerId: string;
  attachment: SessionAttachment;
  leaseId?: string;
  onConnected: () => Promise<string | undefined>;
  onDisconnected: () => void;
  onWorker: (worker: SessionWorker) => void;
  onSnapshot: (snapshot: TerminalSnapshotFrame) => void;
  onError: (message: string) => void;
}

type TerminalServerFrame = TerminalSnapshotFrame
  | { type: 'state'; worker: SessionWorker }
  | { type: 'error'; code: string; message: string };

export function TerminalPanel({ workerId, attachment, leaseId, onConnected, onDisconnected, onWorker, onSnapshot, onError }: TerminalPanelProps) {
  const host = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal>();
  const socketRef = useRef<WebSocket>();
  const leaseRef = useRef(leaseId);
  const [socketState, setSocketState] = useState<SocketState>('connecting');

  useLayoutEffect(() => {
    leaseRef.current = leaseId;
    if (terminalRef.current) terminalRef.current.options.disableStdin = !leaseId;
  }, [leaseId]);

  useLayoutEffect(() => {
    if (!host.current) return;
    let disposed = false;
    let resizeTimer: number | undefined;
    let lastOutputSeq = 0;
    const terminal = new Terminal({
      allowProposedApi: false,
      convertEol: false,
      cursorBlink: true,
      disableStdin: !leaseRef.current,
      fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
      fontSize: 13,
      scrollback: 5_000,
      theme: { background: '#111315', foreground: '#d8dcdf', cursor: '#c9d8ff', selectionBackground: '#52678488' }
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(host.current);
    terminalRef.current = terminal;
    const socket = new WebSocket(terminalWebSocketUrl(workerId), terminalProtocols(attachment.descriptor));
    socket.binaryType = 'arraybuffer';
    socketRef.current = socket;
    const send = (value: object) => {
      if (socket.readyState === WebSocket.OPEN) socket.send(encodeClientFrame(value as never));
    };
    const input = terminal.onData((data) => {
      const activeLease = leaseRef.current;
      if (activeLease) send({ type: 'input', leaseId: activeLease, data });
    });
    const resize = terminal.onResize(({ cols, rows }) => {
      if (!leaseRef.current) return;
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => send({ type: 'resize', cols, rows }), 50);
    });
    const observer = new ResizeObserver(() => {
      try { fit.fit(); } catch { /* xterm may be between layout and disposal */ }
    });
    observer.observe(host.current);
    socket.onopen = () => {
      setSocketState('live');
      fit.fit();
      void onConnected().then((acquiredLeaseId) => {
        if (disposed || !acquiredLeaseId) return;
        leaseRef.current = acquiredLeaseId;
        terminal.options.disableStdin = false;
        terminal.focus();
      });
    };
    socket.onmessage = async (event) => {
      try {
        if (event.data instanceof ArrayBuffer) {
          const frame = decodeOutputFrame(event.data);
          if (frame.outputSeq <= lastOutputSeq) return;
          lastOutputSeq = frame.outputSeq;
          terminal.write(frame.data, () => send({ type: 'ack', outputSeq: frame.outputSeq }));
          return;
        }
        const frame = JSON.parse(String(event.data)) as TerminalServerFrame;
        if (frame.type === 'snapshot') {
          const restored = decodeSnapshot(frame);
          terminal.reset();
          terminal.resize(frame.cols, frame.rows);
          terminal.write(restored.screen);
          terminal.write(restored.replay);
          lastOutputSeq = frame.toSeq;
          onSnapshot(frame);
          send({ type: 'ack', outputSeq: frame.toSeq });
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
      if (!disposed) onDisconnected();
    };

    return () => {
      disposed = true;
      window.clearTimeout(resizeTimer);
      observer.disconnect();
      input.dispose();
      resize.dispose();
      socket.close(1000, 'terminal detached');
      terminal.dispose();
      terminalRef.current = undefined;
      socketRef.current = undefined;
    };
  }, [workerId, attachment.descriptor]);

  return <div class="terminal-panel-wrap">
    <div class={`terminal-transport terminal-transport-${socketState}`} role="status">
      <span />{socketState === 'live' ? 'xterm 已连接' : socketState === 'connecting' ? 'xterm 正在连接' : 'xterm 已断开'}
    </div>
    <div ref={host} class="terminal-panel" aria-label="Codex PTY terminal" />
  </div>;
}
