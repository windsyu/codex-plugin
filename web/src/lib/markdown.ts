import DOMPurify from 'dompurify';
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import markdownLanguage from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import typescript from 'highlight.js/lib/languages/typescript';
import { marked } from 'marked';

marked.setOptions({
  gfm: true,
  breaks: true,
  async: false
});

for (const [name, language] of Object.entries({ bash, javascript, json, markdown: markdownLanguage, python, rust, typescript })) {
  hljs.registerLanguage(name, language);
}

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
