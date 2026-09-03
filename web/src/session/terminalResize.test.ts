import { afterEach, describe, expect, it, vi } from 'vitest';

import { TerminalResizeScheduler } from './terminalResize';

afterEach(() => vi.useRealTimers());

describe('TerminalResizeScheduler', () => {
  it('settles resize bursts and sends only changed dimensions', () => {
    vi.useFakeTimers();
    let dimensions = { cols: 80, rows: 24 };
    const sent: Array<typeof dimensions> = [];
    const scheduler = new TerminalResizeScheduler(
      () => dimensions,
      (next) => { sent.push(next); return true; },
      120
    );

    scheduler.setEnabled(true);
    vi.advanceTimersByTime(60);
    dimensions = { cols: 100, rows: 30 };
    scheduler.schedule();
    vi.advanceTimersByTime(119);
    expect(sent).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(sent).toEqual([{ cols: 100, rows: 30 }]);

    scheduler.schedule();
    vi.advanceTimersByTime(120);
    expect(sent).toHaveLength(1);

    dimensions = { cols: 101, rows: 30 };
    scheduler.schedule();
    vi.advanceTimersByTime(120);
    expect(sent).toEqual([{ cols: 100, rows: 30 }, { cols: 101, rows: 30 }]);
  });

  it('does not resize the PTY while the attachment is read-only', () => {
    vi.useFakeTimers();
    const sent: Array<{ cols: number; rows: number }> = [];
    const scheduler = new TerminalResizeScheduler(
      () => ({ cols: 90, rows: 28 }),
      (next) => { sent.push(next); return true; },
      120
    );

    scheduler.schedule();
    vi.advanceTimersByTime(120);
    scheduler.setEnabled(true);
    scheduler.setEnabled(false);
    vi.advanceTimersByTime(120);

    expect(sent).toEqual([]);
  });

  it('retries a stable size when the socket could not send it', () => {
    vi.useFakeTimers();
    let connected = false;
    const sent: Array<{ cols: number; rows: number }> = [];
    const scheduler = new TerminalResizeScheduler(
      () => ({ cols: 96, rows: 32 }),
      (next) => {
        if (!connected) return false;
        sent.push(next);
        return true;
      },
      120
    );

    scheduler.setEnabled(true);
    vi.advanceTimersByTime(120);
    connected = true;
    scheduler.schedule();
    vi.advanceTimersByTime(120);

    expect(sent).toEqual([{ cols: 96, rows: 32 }]);
  });
});
