import type { ITheme } from '@xterm/xterm';
import profile from './terminalProfile.json';

// Codex TUI intentionally inherits its host terminal. This is the Gateway's dark host palette:
// neutral foreground/background, a clear cyan accent, dim grays, and distinct semantic colors.
// TrueColor sequences emitted by Codex continue to pass through unchanged.
export const CODEX_TERMINAL_PROFILE_ID = profile.id;

export const CODEX_TERMINAL_THEME: ITheme = Object.freeze({
  background: profile.background,
  foreground: profile.foreground,
  cursor: profile.cursor,
  cursorAccent: profile.cursorAccent,
  selectionBackground: profile.selectionBackground,
  selectionForeground: profile.selectionForeground,
  ...profile.ansi
});
