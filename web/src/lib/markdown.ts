import DOMPurify from 'dompurify';
import hljs from 'highlight.js';
import { marked } from 'marked';

marked.setOptions({
  gfm: true,
  breaks: true,
  async: false
});

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A') {
    const href = node.getAttribute('href') || '';
    if (!/^https?:\/\//i.test(href) && !href.startsWith('#')) {
      node.removeAttribute('href');
    }
  }
});

export function renderMarkdown(markdown: string): string {
  const parsed = marked.parse(markdown ?? '') as string;
  const clean = DOMPurify.sanitize(parsed, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ['style', 'form', 'input', 'script', 'iframe', 'object', 'embed']
  });
  const template = document.createElement('template');
  template.innerHTML = clean;
  template.content.querySelectorAll('pre code').forEach((element) => {
    try {
      hljs.highlightElement(element as HTMLElement);
    } catch {
      // Highlighting is best-effort; the already-sanitized text remains visible.
    }
  });
  return template.innerHTML;
}
