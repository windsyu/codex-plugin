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
});
