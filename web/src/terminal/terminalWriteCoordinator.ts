export const INBAND_TERMINAL_RESET = new Uint8Array([0x07, 0x18, 0x1b, 0x63]);

type TerminalWriteData = string | Uint8Array;

export interface TerminalWriter {
  write(data: TerminalWriteData, callback?: () => void): void;
}

interface PendingWrite {
  data: TerminalWriteData;
  beforeWrite?: () => void;
  onWritten?: () => void;
}

function concatBytes(...parts: Uint8Array[]) {
  const length = parts.reduce((total, part) => total + part.byteLength, 0);
  const joined = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.byteLength;
  }
  return joined;
}

// xterm.write is asynchronous internally. Keeping one application-level queue makes an in-band
// reset order behind the one write xterm may already be parsing, while dropping older writes that
// a replacement snapshot supersedes. This avoids terminal.reset() racing xterm's private buffer.
export class TerminalWriteCoordinator {
  private queue: PendingWrite[] = [];
  private active = false;
  private disposed = false;

  constructor(private readonly terminal: TerminalWriter) {}

  write(data: TerminalWriteData, onWritten?: () => void) {
    if (this.disposed || data.length === 0) return;
    this.queue.push({ data, onWritten });
    this.pump();
  }

  barrier(action: () => void) {
    if (this.disposed) return;
    this.queue.push({ data: '', beforeWrite: action });
    this.pump();
  }

  restore(screen: Uint8Array, replay: Uint8Array, beforeWrite: () => void, onWritten?: () => void) {
    if (this.disposed) return;
    this.queue = [{
      data: concatBytes(INBAND_TERMINAL_RESET, screen, replay),
      beforeWrite,
      onWritten
    }];
    this.pump();
  }

  dispose() {
    this.disposed = true;
    this.queue = [];
  }

  private pump() {
    if (this.disposed || this.active) return;
    const next = this.queue.shift();
    if (!next) return;
    this.active = true;
    try {
      next.beforeWrite?.();
      if (next.data.length === 0) {
        this.active = false;
        next.onWritten?.();
        this.pump();
        return;
      }
      this.terminal.write(next.data, () => {
        this.active = false;
        if (this.disposed) return;
        next.onWritten?.();
        this.pump();
      });
    } catch (error) {
      this.active = false;
      this.queue.unshift(next);
      throw error;
    }
  }
}
