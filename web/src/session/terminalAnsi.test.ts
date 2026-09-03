// @vitest-environment node

import { Terminal } from '@xterm/xterm';
import { describe, expect, it } from 'vitest';

function write(terminal: Terminal, data: string) {
  return new Promise<void>((resolve) => terminal.write(data, resolve));
}

describe('xterm Codex ANSI oracle', () => {
  it('preserves the visual attributes emitted by Codex TUI', async () => {
    const terminal = new Terminal({ cols: 40, rows: 4 });
    await write(
      terminal,
      '\x1b[1mB\x1b[0m\x1b[2;3mD\x1b[0m\x1b[36mC\x1b[0m\x1b[38;2;1;2;3mT\x1b[0m\x1b[41mA\x1b[0m\x1b[48;2;4;5;6mR\x1b[0m\x1b[2m─\x1b[0m'
    );
    const line = terminal.buffer.active.getLine(0);

    expect(Boolean(line?.getCell(0)?.isBold())).toBe(true);
    expect(Boolean(line?.getCell(1)?.isDim())).toBe(true);
    expect(Boolean(line?.getCell(1)?.isItalic())).toBe(true);
    expect(line?.getCell(2)?.getFgColor()).toBe(6);
    expect(line?.getCell(3)?.getFgColor()).toBe(0x010203);
    expect(line?.getCell(4)?.getBgColor()).toBe(1);
    expect(line?.getCell(5)?.getBgColor()).toBe(0x040506);
    expect(Boolean(line?.getCell(6)?.isDim())).toBe(true);
    expect(line?.translateToString(true)).toBe('BDCTAR─');
    terminal.dispose();
  });

  it('accepts synchronized output frames used by current Codex TUI', async () => {
    const terminal = new Terminal({ cols: 40, rows: 4 });
    await write(terminal, '\x1b[?2026h\x1b[33mwarning\x1b[0m\x1b[?2026l');

    expect(terminal.buffer.active.getLine(0)?.translateToString(true)).toBe('warning');
    expect(terminal.buffer.active.getLine(0)?.getCell(0)?.getFgColor()).toBe(3);
    terminal.dispose();
  });
});
