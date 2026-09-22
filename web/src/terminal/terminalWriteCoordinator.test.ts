import { describe, expect, it, vi } from 'vitest';

import {
  INBAND_TERMINAL_RESET,
  TerminalWriteCoordinator,
  type TerminalWriter
} from './terminalWriteCoordinator';

class ControlledTerminal implements TerminalWriter {
  writes: Array<string | Uint8Array> = [];
  callbacks: Array<() => void> = [];

  write(data: string | Uint8Array, callback?: () => void) {
    this.writes.push(data);
    if (callback) this.callbacks.push(callback);
  }

  completeNext() {
    const callback = this.callbacks.shift();
    if (!callback) throw new Error('no pending terminal write');
    callback();
  }
}

describe('TerminalWriteCoordinator', () => {
  it('serializes live terminal output', () => {
    const terminal = new ControlledTerminal();
    const coordinator = new TerminalWriteCoordinator(terminal);
    const first = vi.fn();
    const second = vi.fn();

    coordinator.write(new Uint8Array([1]), first);
    coordinator.write(new Uint8Array([2]), second);
    expect(terminal.writes).toEqual([new Uint8Array([1])]);

    terminal.completeNext();
    expect(first).toHaveBeenCalledOnce();
    expect(terminal.writes).toEqual([new Uint8Array([1]), new Uint8Array([2])]);
    terminal.completeNext();
    expect(second).toHaveBeenCalledOnce();
  });

  it('replaces queued stale output with an ordered in-band snapshot reset', () => {
    const terminal = new ControlledTerminal();
    const coordinator = new TerminalWriteCoordinator(terminal);
    const stale = vi.fn();
    const resize = vi.fn();
    const restored = vi.fn();

    coordinator.write(new Uint8Array([1]));
    coordinator.write(new Uint8Array([2]), stale);
    coordinator.restore(new Uint8Array([3]), new Uint8Array([4]), resize, restored);
    coordinator.write(new Uint8Array([5]));

    terminal.completeNext();
    expect(stale).not.toHaveBeenCalled();
    expect(resize).toHaveBeenCalledOnce();
    expect(terminal.writes[1]).toEqual(
      new Uint8Array([...INBAND_TERMINAL_RESET, 3, 4])
    );

    terminal.completeNext();
    expect(restored).toHaveBeenCalledOnce();
    expect(terminal.writes[2]).toEqual(new Uint8Array([5]));
  });

  it('drops queued work after disposal without reviving from a late callback', () => {
    const terminal = new ControlledTerminal();
    const coordinator = new TerminalWriteCoordinator(terminal);
    const queued = vi.fn();

    coordinator.write('active');
    coordinator.write('queued', queued);
    coordinator.dispose();
    terminal.completeNext();

    expect(terminal.writes).toEqual(['active']);
    expect(queued).not.toHaveBeenCalled();
  });

  it('applies resize barriers after pending parsing and before the next output', () => {
    const terminal = new ControlledTerminal();
    const coordinator = new TerminalWriteCoordinator(terminal);
    const resize = vi.fn();
    coordinator.write('old dimensions');
    coordinator.barrier(resize);
    coordinator.write('new dimensions');
    expect(resize).not.toHaveBeenCalled();
    terminal.completeNext();
    expect(resize).toHaveBeenCalledOnce();
    expect(terminal.writes).toEqual(['old dimensions', 'new dimensions']);
  });
});
