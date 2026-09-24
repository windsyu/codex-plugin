import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Terminal } from '@xterm/xterm';
import type { FitAddon } from '@xterm/addon-fit';
import { TerminalClient, base64FromText, bytesFromBase64, type TerminalView } from './terminalClient';

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = 1; bufferedAmount = 0; sent: any[] = [];
  onmessage?: (event: { data: string }) => void; onclose?: () => void;
  constructor(_url: string) { Socket.instances.push(this); }
  send(text: string) { this.sent.push(JSON.parse(text)); }
  close() { this.readyState = 3; this.onclose?.(); }
  receive(frame: object) { this.onmessage?.({ data: JSON.stringify({ runEpoch: 'epoch', ...frame }) }); }
}
let client: TerminalClient | undefined;
beforeEach(() => { Socket.instances = []; sessionStorage.clear(); vi.stubGlobal('WebSocket', Socket); });
afterEach(() => { client?.dispose(); client = undefined; vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });
function fixture(control = { controllerConnection: null as string | null, generation: 0, reconnectReserved: false, ended: false, rows: 24, cols: 80 }, deferWrites = false) {
  let view: TerminalView;
  const writes: (() => void)[] = [];
  const terminal = { options: {}, resize: vi.fn(), focus: vi.fn(), write: (_data: unknown, done: () => void) => { if (deferWrites) writes.push(done); else done(); } };
  const fit = { proposeDimensions: () => ({ rows: 24, cols: 80 }) };
  client = new TerminalClient(terminal as unknown as Terminal, fit as FitAddon, 'epoch', value => { view = value; });
  const socket = Socket.instances.at(-1)!;
  socket.receive({ type: 'snapshot', connectionId: 'this-page', outputSeq: 0, screen: '', replay: '', rows: 24, cols: 80, complete: true, truncated: false, exit: null,
    control });
  return { socket, terminal, fit, view: () => view!, flush: () => writes.shift()?.() };
}

describe('native terminal browser connection', () => {
  it('reports an exit only from the server, preserves it on disconnect and never reclaims ended input', () => {
    const { socket, view, terminal } = fixture();
    socket.close();
    expect(view().exit).toBeNull();
    const exit = { code: 0, signal: null };
    socket.receive({ type: 'state', exit, control: { controllerConnection: null, generation: 2, reconnectReserved: false, ended: true, rows: 24, cols: 80 } });
    expect(view().exit).toEqual(exit);
    expect(terminal.options).toMatchObject({ disableStdin: true });
    const before = socket.sent.length;
    client!.input('after exit'); client!.takeover(); socket.close();
    expect(socket.sent).toHaveLength(before);
    expect(view().exit).toEqual(exit);
  });
  it('automatically enables a free terminal without trusting copied credentials or sending input early', () => {
    sessionStorage.setItem('workbench-reconnect:epoch', 'copied-token');
    const { socket, view } = fixture();
    expect(view().owned).toBe(false);
    expect(socket.sent.map(frame => frame.command)).toEqual([{ type: 'claim' }]);
    client!.input('must not send');
    expect(socket.sent.map(frame => frame.command)).toEqual([{ type: 'claim' }]);
    socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    expect(view().owned).toBe(true);
    client!.input('ready');
    expect(socket.sent.filter(frame => frame.command.type === 'input')).toHaveLength(1);
  });
  it('does not take another page over automatically, but lets the user switch here once', () => {
    const control = { controllerConnection: 'other-page', generation: 1, reconnectReserved: false, ended: false, rows: 24, cols: 80 };
    sessionStorage.setItem('workbench-reconnect:epoch', 'copied-token');
    const { socket, view } = fixture(control);
    expect(socket.sent).toEqual([]); expect(view().owned).toBe(false);
    client!.input('blocked'); expect(socket.sent).toEqual([]);
    client!.takeover(); client!.takeover();
    expect(socket.sent.map(frame => frame.command)).toEqual([{ type: 'takeover', confirmed: true }]);
  });
  it('waits for the rendered snapshot and respects an owner learned during restoration', () => {
    const { socket, view, flush } = fixture(undefined, true);
    expect(view().ready).toBe(false); expect(socket.sent).toEqual([]);
    client!.input('before screen restoration');
    socket.receive({ type: 'state', control: { controllerConnection: 'other-page', generation: 1, reconnectReserved: false, ended: false, rows: 24, cols: 80 } });
    flush();
    expect(view().ready).toBe(true); expect(view().owned).toBe(false);
    expect(socket.sent).toEqual([]);
  });
  it('waits for a reconnect reservation, then claims the vacant terminal once without polling', () => {
    const control = { controllerConnection: null, generation: 2, reconnectReserved: true, ended: false, rows: 24, cols: 80 };
    const { socket } = fixture(control); expect(socket.sent).toEqual([]);
    const available = { type: 'state', control: { ...control, generation: 3, reconnectReserved: false } };
    socket.receive(available); socket.receive(available);
    expect(socket.sent.map(frame => frame.command)).toEqual([{ type: 'claim' }]);
    socket.receive({ type: 'error', id: 1, error: { code: { control: 'input_held' } } });
    socket.receive(available);
    expect(socket.sent.map(frame => frame.command)).toEqual([{ type: 'claim' }]);
  });
  it('does not claim an ended terminal or replay input after an expired reconnect credential', () => {
    const ended = fixture({ controllerConnection: null, generation: 3, reconnectReserved: false, ended: true, rows: 24, cols: 80 });
    expect(ended.socket.sent).toEqual([]); client!.dispose();
    vi.spyOn(performance, 'getEntriesByType').mockReturnValue([{ type: 'reload' }] as unknown as PerformanceEntry[]);
    sessionStorage.setItem('workbench-reconnect:epoch', 'expired-token');
    const { socket } = fixture();
    expect(socket.sent.map(frame => frame.command.type)).toEqual(['reconnect']);
    socket.receive({ type: 'error', id: 1, error: { code: { control: 'reconnect_rejected' } } });
    expect(socket.sent.map(frame => frame.command.type)).toEqual(['reconnect', 'claim']);
  });
  it('ignores a late public state older than the private grant', () => {
    const { socket, view } = fixture();
    socket.receive({ type: 'grant', id: 1, grant: { generation: 3, reconnectSecret: 'a'.repeat(64) } });
    socket.receive({ type: 'state', control: { controllerConnection: 'old-page', generation: 2, rows: 24, cols: 80, ended: false } });
    expect(view().owned).toBe(true);
    client!.input('中文\n');
    const input = socket.sent.find(frame => frame.command.type === 'input');
    expect(input.command.sequence).toBe(1);
    expect(new TextDecoder().decode(bytesFromBase64(input.command.data))).toBe('中文\n');
    socket.receive({ type: 'state', control: { controllerConnection: 'new-page', generation: 4, rows: 24, cols: 80, ended: false } });
    expect(view().owned).toBe(false);
    const before = socket.sent.length;
    client!.input('stale'); expect(socket.sent).toHaveLength(before);
    expect(sessionStorage.getItem('workbench-reconnect:epoch')).toBeNull();
  });
  it('never resends uncertain input when the socket reconnects', () => {
    vi.useFakeTimers();
    const { socket, view } = fixture();
    socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    client!.input('unacknowledged'); socket.close();
    expect(view().inputUncertain).toBe(true);
    vi.advanceTimersByTime(750);
    const next = Socket.instances[1];
    expect(next.sent).toEqual([]);
    next.receive({ type: 'snapshot', connectionId: 'refreshed', outputSeq: 1, screen: '', replay: '', rows: 24, cols: 80, complete: true, control: { ended: false } });
    expect(next.sent.map(frame => frame.command.type)).toEqual(['reconnect']);
    next.receive({ type: 'grant', id: 1, grant: { generation: 2, reconnectSecret: 'b'.repeat(64) } });
    expect(view().inputUncertain).toBe(true);
    expect(next.sent.filter(frame => frame.command.type === 'input')).toEqual([]);
    expect(sessionStorage.getItem('workbench-pending-input:epoch')).toBe('1');
    const sent = next.sent.length;
    client!.acknowledgeInputUncertainty();
    expect(view().inputUncertain).toBe(false);
    expect(sessionStorage.getItem('workbench-pending-input:epoch')).toBeNull();
    expect(next.sent).toHaveLength(sent);
  });
  it('retains a pending-input warning across reload without persisting or resending any input bytes', () => {
    const first = fixture();
    first.socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    client!.input('private synthetic draft'); client!.dispose();
    vi.spyOn(performance, 'getEntriesByType').mockReturnValue([{ type: 'reload' }] as unknown as PerformanceEntry[]);
    const reloaded = fixture(); const next = reloaded.socket;
    expect(next.sent.map(frame => frame.command.type)).toEqual(['reconnect']);
    expect(reloaded.view().inputUncertain).toBe(true);
    expect(sessionStorage.getItem('workbench-pending-input:epoch')).toBe('1');
    expect(JSON.stringify(sessionStorage)).not.toContain('private synthetic draft');
  });
  it('keeps input available after partial I/O failure without replay or another grant, and retains the warning', () => {
    const { socket, view } = fixture();
    socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    client!.input('confirmed'); const id = socket.sent.find(frame => frame.command.type === 'input').id;
    socket.receive({ type: 'ack', id });
    expect(sessionStorage.getItem('workbench-pending-input:epoch')).toBeNull();
    client!.input('partly written');
    socket.receive({ type: 'error', id: socket.sent.at(-1).id, error: { code: { control: 'pty_write_failed' } } });
    expect(view().owned).toBe(true); expect(view().inputUncertain).toBe(true);
    expect(sessionStorage.getItem('workbench-reconnect:epoch')).toBe('a'.repeat(64));
    const count = socket.sent.length;
    client!.input('new input');
    expect(socket.sent).toHaveLength(count + 1);
    expect(socket.sent.at(-1).command).toMatchObject({ type: 'input', generation: 1, sequence: 3, data: base64FromText('new input') });
    socket.receive({ type: 'grant', id: 3, grant: { generation: 3, reconnectSecret: 'c'.repeat(64) } });
    expect(view().owned).toBe(true); expect(view().inputUncertain).toBe(true);
  });
  it('bounds pasted UTF-8 bytes before allocating a protocol frame', () => {
    expect(() => base64FromText('中'.repeat(23000))).toThrow('64KiB');
    expect(new TextDecoder().decode(bytesFromBase64(base64FromText('a\u0003中文\n')))).toBe('a\u0003中文\n');
  });

  it('ignores non-finite hidden-terminal dimensions without sending an invalid resize', () => {
    const { socket, fit } = fixture();
    socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    const before = socket.sent.length;
    fit.proposeDimensions = () => ({ rows: NaN, cols: NaN });
    client!.resize();
    expect(socket.sent).toHaveLength(before);
  });

  it('keeps a screen failure visible after a new input grant while output continues', () => {
    const { socket, terminal, view } = fixture();
    socket.receive({ type: 'fault', error: { code: 'screen_state_unavailable' } });
    socket.receive({ type: 'grant', id: 1, grant: { generation: 1, reconnectSecret: 'a'.repeat(64) } });
    expect(view().screenUnavailable).toBe(true);
    expect(view().owned).toBe(true);
    const write = vi.spyOn(terminal, 'write');
    socket.receive({ type: 'output', outputSeq: 1, data: base64FromText('后续输出') });
    expect(new TextDecoder().decode(write.mock.calls[0][0] as Uint8Array)).toBe('后续输出');
    expect(view().screenUnavailable).toBe(true);
  });
});
