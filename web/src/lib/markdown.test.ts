import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown', () => {
  it('renders basic markdown', () => {
    expect(renderMarkdown('**hello**')).toContain('<strong>hello</strong>');
  });

  it('strips scripts and unsafe elements', () => {
    const html = renderMarkdown('<script>alert(1)</script><p>safe</p>');
    expect(html).not.toContain('<script');
    expect(html).toContain('safe');
  });

  it('does not turn local file paths into executable links', () => {
    const html = renderMarkdown('[file](/etc/passwd)');
    expect(html).not.toContain('href="/etc/passwd"');
  });

  it('removes javascript, data and file links while preserving safe web links', () => {
    const html = renderMarkdown('[js](javascript:alert(1)) [data](data:text/html,bad) [file](file:///tmp/a) [web](https://example.com)');
    expect(html).not.toMatch(/href="(?:javascript:|data:|file:)/i);
    expect(html).toContain('href="https://example.com"');
  });

  it('does not execute svg, html handlers or ANSI control text', () => {
    const html = renderMarkdown('<svg onload="alert(1)"><script>alert(2)</script></svg>\n\u001b[31mred');
    expect(html).not.toContain('onload');
    expect(html).not.toContain('<script');
    expect(html).toContain('red');
  });
});
