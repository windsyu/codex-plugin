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
    expect(css).toMatch(/\.thread-row, \.search-result, \.back-button[^}]*min-height:\s*44px/);
  });
});
