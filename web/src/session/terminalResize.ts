export interface TerminalDimensions {
  cols: number;
  rows: number;
}

export type TerminalResizeSender = (dimensions: TerminalDimensions) => boolean;

export const TERMINAL_RESIZE_SETTLE_MS = 120;

function sameDimensions(left: TerminalDimensions | undefined, right: TerminalDimensions) {
  return Boolean(left && left.cols === right.cols && left.rows === right.rows);
}

export class TerminalResizeScheduler {
  private enabled = false;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private lastSent: TerminalDimensions | undefined;

  constructor(
    private readonly readDimensions: () => TerminalDimensions,
    private readonly send: TerminalResizeSender,
    private readonly settleMs = TERMINAL_RESIZE_SETTLE_MS
  ) {}

  setEnabled(enabled: boolean) {
    this.enabled = enabled;
    if (!enabled) {
      this.clearTimer();
      return;
    }
    this.schedule();
  }

  schedule() {
    if (!this.enabled) return;
    this.clearTimer();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      const dimensions = this.readDimensions();
      if (sameDimensions(this.lastSent, dimensions)) return;
      if (this.send(dimensions)) this.lastSent = { ...dimensions };
    }, this.settleMs);
  }

  dispose() {
    this.enabled = false;
    this.clearTimer();
  }

  private clearTimer() {
    if (this.timer === undefined) return;
    clearTimeout(this.timer);
    this.timer = undefined;
  }
}
