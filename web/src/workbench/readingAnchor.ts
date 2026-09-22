// Keep the visible card at its viewport position when a late native user event
// is inserted above it. Stable keys also survive snapshots and text replacement.
export interface ReadingAnchor { key: string; offset: number }
export function readAnchor(list: HTMLElement): ReadingAnchor | null {
  const top = list.getBoundingClientRect().top;
  for (const item of list.querySelectorAll<HTMLElement>('[data-reading-key]')) {
    const rect = item.getBoundingClientRect();
    if (rect.bottom > top) return { key: item.dataset.readingKey!, offset: rect.top - top };
  }
  return null;
}
export function restoreAnchor(list: HTMLElement, anchor: ReadingAnchor | null) {
  if (!anchor) return;
  const item = [...list.querySelectorAll<HTMLElement>('[data-reading-key]')].find(item => item.dataset.readingKey === anchor.key);
  if (item) list.scrollTop += item.getBoundingClientRect().top - list.getBoundingClientRect().top - anchor.offset;
}
