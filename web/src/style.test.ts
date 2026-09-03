import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

const css = readFileSync(`${process.cwd()}/src/style.css`, 'utf8');

describe('responsive Viewer contract', () => {
  it('switches between list and detail at the 820px breakpoint', () => {
    expect(css).toContain('@media (max-width: 820px)');
    expect(css).toMatch(/\.workspace\.detail-active \.sidebar\s*\{\s*display:\s*none/);
    expect(css).toMatch(/\.workspace\.detail-active \.detail\s*\{\s*display:\s*block/);
  });

  it('bounds page overflow and preserves local code/raw scrolling', () => {
    expect(css).toMatch(/html, body\s*\{[^}]*overflow-x:\s*hidden/);
    expect(css).toMatch(/\.raw-event pre\s*\{[^}]*overflow:\s*auto/);
    expect(css).toMatch(/\.markdown pre\s*\{[^}]*overflow:\s*auto/);
  });

  it('defines keyboard focus and mobile target sizes', () => {
    expect(css).toContain(':focus-visible');
    expect(css).toMatch(/\.project-heading, \.subagent-group > summary, \.thread-row, \.search-result, \.back-button[^}]*min-height:\s*44px/);
  });

  it('uses a compact Codex-like application shell and centered conversation canvas', () => {
    expect(css).toMatch(/\.topbar\s*\{[^}]*min-height:\s*52px/);
    expect(css).toMatch(/\.workspace\s*\{[^}]*grid-template-columns:\s*320px/);
    expect(css).toMatch(/\.detail-inner\s*\{[^}]*max-width:\s*900px/);
    expect(css).toMatch(/\.conversation-dock\s*\{[^}]*position:\s*sticky/);
    expect(css).toMatch(/\.composer\s*\{[^}]*border-radius:\s*22px/);
    expect(css).toMatch(/\.jump-to-latest\s*\{[^}]*border-radius:\s*99px/);
    expect(css).toMatch(/\.session-overview\s*\{[^}]*flex-wrap:\s*wrap/);
    expect(css).toMatch(/\.dialogue-assistant\s*\{[^}]*grid-template-columns:\s*28px/);
    expect(css).toMatch(/\.project-heading\s*\{[^}]*justify-content:\s*flex-start/);
    expect(css).toMatch(/\.project-heading \.project-title\s*\{[^}]*flex:\s*1/);
  });

  it('keeps the native terminal chrome stable and measures a padding-free xterm host', () => {
    expect(css).toMatch(/\.session-toolbar\s*\{[^}]*white-space|\.session-status-cluster\s*\{[^}]*white-space:\s*nowrap/);
    expect(css).toMatch(/\.session-context-strip\s*\{[^}]*white-space:\s*nowrap/);
    expect(css).toMatch(/\.terminal-panel-wrap\s*\{[^}]*display:\s*flex/);
    expect(css).toMatch(/\.terminal-stage\s*\{[^}]*padding:/);
    expect(css).toMatch(/\.terminal-panel\s*\{[^}]*height:\s*100%/);
    expect(css).not.toMatch(/\.terminal-panel\s*\{[^}]*padding:/);
    expect(css).toMatch(/\.xterm-viewport\s*\{[^}]*scrollbar-gutter:\s*stable/);
  });

  it('keeps a single compact runtime status row outside the xterm canvas', () => {
    expect(css).toMatch(/\.terminal-titlebar\s*\{[^}]*flex:\s*none/);
    expect(css).toMatch(/\.terminal-runtime-status\s*\{[^}]*white-space:\s*nowrap/);
    expect(css).toMatch(/\.terminal-runtime-status\s*\{[^}]*overflow-x:\s*auto/);
    expect(css).toMatch(/\.terminal-panel-wrap:focus-within\s*\{[^}]*border-color:/);
    expect(css).toMatch(/\.terminal-input-status\s*\{[^}]*flex:\s*none/);
    expect(css).toMatch(/\.terminal-input-status-attention\s*\{[^}]*border-color:/);
    expect(css).not.toContain('.terminal-legend');
    expect(css).not.toContain('.terminal-input-guide');
  });
});
