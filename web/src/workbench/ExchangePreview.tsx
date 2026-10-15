import { useMemo } from 'preact/hooks';
import { renderMarkdown } from '../lib/markdown';
import type { LibraryEntry } from './library';

// This is a display excerpt, not a generated summary. Never mount the source
// HTML or media, and retain code text when no prose was saved in the excerpt.
export function exchangePreviewText(text: string): string {
  const plain = (input: string, proseOnly: boolean) => {
    const template = document.createElement('template');
    template.innerHTML = renderMarkdown(input);
    template.content.querySelectorAll('img,video,audio,source,svg,script,style').forEach(node => node.remove());
    if (proseOnly) template.content.querySelectorAll('pre').forEach(node => node.remove());
    return Array.from(template.content.childNodes, node => node.textContent || '').join(' ').replace(/\s+/g, ' ').trim();
  };
  // The bounded catalog excerpt may flatten line breaks or end inside a fence.
  // Inline identifiers remain meaningful prose; only fenced blocks are skipped.
  const prose = plain(text.replace(/(`{3,}|~{3,})[\s\S]*?(?:\1|$)/g, ' '), true);
  return prose || plain(text, false);
}

export function ExchangePreview({ preview }: { preview: LibraryEntry['preview'] }) {
  const text = useMemo(() => preview?.text ? exchangePreviewText(preview.text) : '', [preview?.text]);
  return text ? <small className="wb-library-preview" title="当前来源已索引的最近有效交流">{preview?.role === 'user' ? '你：' : '回复：'}{text}</small> : null;
}
