import { describe, expect, it } from 'vitest';

import { CODEX_TERMINAL_PROFILE_ID, CODEX_TERMINAL_THEME } from './terminalTheme';

describe('Codex terminal host palette', () => {
  it('provides the dark host colors that Codex TUI expects to inherit', () => {
    expect(CODEX_TERMINAL_PROFILE_ID).toBe('codex-dark-v1');
    expect(CODEX_TERMINAL_THEME).toMatchObject({
      background: '#0d0f10',
      foreground: '#e8eaed',
      cursor: '#f7f7f8',
      cyan: '#56b6c2',
      brightBlack: '#747a80'
    });
  });

  it('defines every ANSI system color instead of falling back to xterm defaults', () => {
    for (const color of [
      'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
      'brightBlack', 'brightRed', 'brightGreen', 'brightYellow',
      'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'
    ] as const) expect(CODEX_TERMINAL_THEME[color]).toMatch(/^#[0-9a-f]{6}$/i);
  });
});
