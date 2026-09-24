import type { Terminal } from '@xterm/xterm';
import type { FitAddon } from '@xterm/addon-fit';
import { TerminalWriteCoordinator } from '../terminal/terminalWriteCoordinator';
import { runApi } from './runApi';

export interface Control {
  controllerConnection: string | null; generation: number; reconnectReserved: boolean;
  ended: boolean; rows: number; cols: number;
}
export interface TerminalView {
  connected: boolean; ready: boolean; owned: boolean; control: Control | null;
  inputUncertain: boolean;
  issue: string; partial: boolean; screenUnavailable: boolean; exit: { code: number; signal: string | null } | null;
}
export const initialTerminalView = (): TerminalView => ({ connected: false, ready: false, owned: false, control: null, inputUncertain: false, issue: '', partial: false, screenUnavailable: false, exit: null });
export function bytesFromBase64(data: string): Uint8Array {
  return Uint8Array.from(atob(data), c => c.charCodeAt(0));
}
export function base64FromText(data: string): string {
  const bytes = new TextEncoder().encode(data);
  if (bytes.length > 64 * 1024) throw new Error('一次粘贴最多 64KiB，请分段输入。');
  return btoa(Array.from(bytes, byte => String.fromCharCode(byte)).join(''));
}

// Connection-local IDs are never accepted from the page as actor identity.
export class TerminalClient {
  private socket?: WebSocket;
  private timer?: ReturnType<typeof setTimeout>;
  private disposed = false;
  private connection = '';
  private request = 0;
  private inputSeq = 0;
  private outputSeq = 0;
  private generation?: number;
  private secret?: string;
  private focusOnGrant = false;
  private controlRequest?: number;
  private autoClaimGeneration?: number;
  private pendingInputs = new Set<number>();
  private writer: TerminalWriteCoordinator;
  private queuedBytes = 0;
  private restoreGeneration = 0;
  private lastResize = '';
  private view = initialTerminalView();
  private readonly storageKey: string;
  private readonly uncertaintyKey: string;

  constructor(private readonly terminal: Terminal, private readonly fit: FitAddon, private readonly epoch: string,
    private readonly changed: (view: TerminalView) => void) {
    this.writer = new TerminalWriteCoordinator(terminal);
    this.storageKey = `workbench-reconnect:${epoch}`;
    this.uncertaintyKey = `workbench-pending-input:${epoch}`;
    // A newly opened/duplicated tab must not silently take authority from the
    // source tab merely because its sessionStorage was copied by the browser.
    const navigation = performance.getEntriesByType?.('navigation')?.[0] as PerformanceNavigationTiming | undefined;
    try {
      if (navigation?.type === 'reload') {
        this.secret = sessionStorage.getItem(this.storageKey) || undefined;
        this.view.inputUncertain = sessionStorage.getItem(this.uncertaintyKey) === '1';
      } else {
        sessionStorage.removeItem(this.storageKey);
        sessionStorage.removeItem(this.uncertaintyKey);
      }
    } catch { /* A free terminal can still connect without browser storage. */ }
    this.open();
  }
  private update(change: Partial<TerminalView>) {
    this.view = { ...this.view, ...change };
    this.terminal.options.disableStdin = !this.view.owned || !this.view.ready;
    this.changed(this.view);
  }
  private remember(secret?: string) {
    this.secret = secret;
    try {
      if (secret) sessionStorage.setItem(this.storageKey, secret);
      else sessionStorage.removeItem(this.storageKey);
    } catch { /* In-page reconnect remains available without sessionStorage. */ }
  }
  private rememberInputState() {
    // Only a pending flag, never draft text or bytes. A reload cannot determine
    // whether a previously sent input reached the native CLI without its ACK.
    try {
      if (this.view.inputUncertain || this.pendingInputs.size) sessionStorage.setItem(this.uncertaintyKey, '1');
      else sessionStorage.removeItem(this.uncertaintyKey);
    } catch { /* In-page uncertainty remains visible if storage is unavailable. */ }
  }
  acknowledgeInputUncertainty() {
    this.update({ inputUncertain: false });
    this.rememberInputState();
  }
  private send(command: object): number | undefined {
    if (this.disposed || this.socket?.readyState !== WebSocket.OPEN || !this.view.ready) return;
    if (this.socket.bufferedAmount > 128 * 1024) {
      this.update({ issue: '终端连接拥堵，本次输入未发送，请检查草稿。' });
      return;
    }
    const id = ++this.request;
    this.socket.send(JSON.stringify({ runEpoch: this.epoch, id, command }));
    return id;
  }
  private autoClaim() {
    const control = this.view.control;
    if (!this.view.connected || !this.view.ready || this.view.owned || this.controlRequest !== undefined
      || !control || control.ended || control.controllerConnection !== null || control.reconnectReserved
      || this.autoClaimGeneration === control.generation) return;
    this.controlRequest = this.send({ type: 'claim' });
    if (this.controlRequest !== undefined) this.autoClaimGeneration = control.generation;
  }
  takeover() {
    if (this.controlRequest !== undefined || this.view.owned || !this.view.ready || this.view.control?.ended) return;
    this.focusOnGrant = true;
    this.controlRequest = this.send({ type: 'takeover', confirmed: true });
  }
  input(text: string) {
    if (!this.view.owned || !this.view.ready || this.generation === undefined) return;
    try {
      const data = base64FromText(text);
      if (!data) return;
      const id = this.send({ type: 'input', generation: this.generation, sequence: this.inputSeq + 1, data });
      if (id !== undefined) { this.inputSeq++; this.pendingInputs.add(id); this.rememberInputState(); }
    } catch (error) { this.update({ issue: (error as Error).message }); }
  }
  resize() {
    if (this.disposed || !this.view.owned || !this.view.ready || this.generation === undefined) return;
    const dimensions = this.fit.proposeDimensions();
    if (!dimensions || !Number.isFinite(dimensions.cols) || !Number.isFinite(dimensions.rows)
      || dimensions.cols < 20 || dimensions.rows < 2) return;
    const rows = Math.min(200, dimensions.rows), cols = Math.min(500, dimensions.cols);
    const size = `${rows}:${cols}`;
    if (size === this.lastResize) return;
    if (this.send({ type: 'resize', generation: this.generation, rows, cols }) !== undefined) this.lastResize = size;
  }
  private control(control: Control) {
    if (control.generation < Math.max(this.view.control?.generation || 0, this.generation || 0)) return;
    const owned = control.controllerConnection === this.connection && control.generation === this.generation && !control.ended;
    if (!owned && this.generation !== undefined) { this.generation = undefined; this.remember(); }
    this.writer.barrier(() => this.terminal.resize(control.cols, control.rows));
    this.update({ control, owned });
    if (owned) this.resize(); else this.autoClaim();
  }
  private open() {
    if (this.disposed) return;
    const socket = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}${runApi(`/terminal?epoch=${encodeURIComponent(this.epoch)}`)}`);
    this.socket = socket;
    this.request = 0; this.inputSeq = 0; this.generation = undefined; this.lastResize = '';
    this.controlRequest = undefined; this.autoClaimGeneration = undefined;
    socket.onmessage = event => {
      if (this.socket !== socket || this.disposed) return;
      try { this.receive(JSON.parse(event.data)); }
      catch { this.update({ issue: '终端画面需要重新同步。' }); socket.close(); }
    };
    socket.onclose = () => {
      if (this.socket !== socket || this.disposed) return;
      const uncertain = this.view.inputUncertain || this.pendingInputs.size > 0;
      this.pendingInputs.clear();
      this.generation = undefined;
      this.update({ connected: false, ready: false, owned: false, inputUncertain: uncertain });
      this.rememberInputState();
      this.timer = setTimeout(() => this.open(), 750);
    };
    socket.onerror = () => { /* onclose performs one bounded reconnect. */ };
  }
  private receive(frame: any) {
    if (frame.runEpoch !== this.epoch) throw new Error('stale terminal run');
    switch (frame.type) {
      case 'snapshot': {
        this.connection = frame.connectionId;
        this.outputSeq = frame.outputSeq;
        this.generation = undefined;
        this.update({ connected: true, ready: false, owned: false, partial: !frame.complete || frame.truncated, exit: frame.exit });
        this.view.control = frame.control;
        const screen = bytesFromBase64(frame.screen), replay = bytesFromBase64(frame.replay);
        const restore = ++this.restoreGeneration;
        this.queuedBytes = screen.length + replay.length;
        this.writer.restore(screen, replay, () => this.terminal.resize(frame.cols, frame.rows), () => {
          if (restore !== this.restoreGeneration || this.disposed) return;
          this.queuedBytes -= screen.length + replay.length;
          this.update({ ready: true });
          if (this.secret && !this.view.control?.ended) this.controlRequest = this.send({ type: 'reconnect', secret: this.secret });
          else this.autoClaim();
        });
        if (frame.fault) this.fault(frame.fault.code);
        break;
      }
      case 'output': {
        if (frame.outputSeq <= this.outputSeq) return;
        if (frame.outputSeq !== this.outputSeq + 1) throw new Error('terminal output gap');
        const bytes = bytesFromBase64(frame.data);
        if (this.queuedBytes + bytes.length > 2 * 1024 * 1024) throw new Error('terminal render capacity');
        const restore = this.restoreGeneration;
        this.queuedBytes += bytes.length; this.outputSeq = frame.outputSeq;
        this.writer.write(bytes, () => { if (restore === this.restoreGeneration) this.queuedBytes -= bytes.length; });
        break;
      }
      case 'state': this.update({ exit: frame.exit }); this.control(frame.control); break;
      case 'grant':
        this.controlRequest = undefined;
        this.generation = frame.grant.generation; this.inputSeq = 0; this.lastResize = '';
        this.remember(frame.grant.reconnectSecret); this.update({ owned: true, issue: '' });
        this.resize();
        if (this.focusOnGrant) { this.focusOnGrant = false; this.terminal.focus(); }
        break;
      case 'ack': this.pendingInputs.delete(frame.id); this.rememberInputState(); break;
      case 'error': {
        if (this.controlRequest === frame.id) this.controlRequest = undefined;
        this.pendingInputs.delete(frame.id);
        const code = frame.error?.code?.control || frame.code;
        // The actor consumes failed input sequences but retains authority on
        // PTY I/O failure. Keep fresh input available without replaying bytes
        // or requiring the user to acquire the same terminal again.
        if (code === 'pty_write_failed' || code === 'pty_resize_failed') {
          this.lastResize = '';
          this.update({ inputUncertain: this.view.inputUncertain || code === 'pty_write_failed',
            issue: code === 'pty_resize_failed' ? '终端尺寸调整失败，请检查终端连接状态。' : '' });
          this.rememberInputState();
          break;
        }
        this.generation = undefined; this.remember(); this.lastResize = '';
        this.update({ owned: false, issue: code === 'reconnect_rejected' || code === 'input_held' ? ''
            : code === 'ended' ? '原生 CLI 已结束。' : '操作未接受，请检查终端连接状态。' });
        this.rememberInputState();
        if (code === 'reconnect_rejected') this.autoClaim();
        break;
      }
      case 'fault': this.fault(frame.error?.code); break;
      case 'snapshot_required': throw new Error('terminal resnapshot');
      default: throw new Error('unknown terminal frame');
    }
  }
  private fault(code: string) {
    if (code === 'screen_state_unavailable') this.update({ screenUnavailable: true });
    else this.update({ issue: '终端读取或协议应答异常，画面可能不完整。' });
  }
  dispose() {
    this.disposed = true;
    clearTimeout(this.timer);
    this.writer.dispose();
    this.socket?.close();
  }
}
