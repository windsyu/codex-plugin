import { render } from 'preact';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ContextSummaryCard } from './ContextSummaryCard';
import { contextKinds, validContextPresentation, type ContextPresentation } from './contextSummary';

const root = document.createElement('div');
const summary = (overrides: Partial<ContextPresentation> = {}): ContextPresentation => ({
  schemaVersion: 1, contentBytes: 40, estimatedTokens: 10, tokens: 100, calibrated: true, reportedInputTokens: 100,
  coverage: { complete: true, issues: [] },
  parts: contextKinds.map(kind => ({ kind, contentBytes: kind === 'files' ? 40 : 0, estimatedTokens: kind === 'files' ? 10 : 0, tokens: kind === 'files' ? 100 : 0, itemCount: kind === 'files' ? 1 : 0 })),
  items: [{ id: 'item/1', kind: 'files', name: '<img src=x onerror=alert(1)>', tag: '<svg>', contentBytes: 40, estimatedTokens: 10, tokens: 100, occurrences: 2, references: ['/input/0'], referencesPartial: true }], ...overrides,
});
afterEach(() => render(null, root));
describe('context summary presentation', () => {
  it('uses server tokens and escapes names while providing category and item controls', () => {
    const onCategory = vi.fn(), onItem = vi.fn();
    render(<ContextSummaryCard summary={summary()} onCategory={onCategory} onItem={onItem} />, root);
    expect(root.textContent).toContain('按上报输入量校准的估算');
    expect(root.textContent).toContain('100 token · 100.0%');
    expect(root.textContent).toContain('<img src=x onerror=alert(1)>');
    expect(root.textContent).toContain('引用不完整');
    expect(root.querySelector('img,svg')).toBeNull();
    expect(root.querySelectorAll('.wb-context-part')).toHaveLength(7);
    (root.querySelector('.wb-context-part.is-files') as HTMLButtonElement).click();
    expect(onCategory).toHaveBeenCalledWith('files');
    (root.querySelector('.wb-context-items button') as HTMLButtonElement).click();
    expect(onItem).toHaveBeenCalledWith('item/1');
    render(<ContextSummaryCard summary={summary()} category="tools" onCategory={onCategory} onItem={onItem} />, root);
    expect(root.querySelectorAll('.wb-context-items button')).toHaveLength(0);
    expect(root.textContent).toContain('此分类没有已捕获的贡献项');
  });
  it('keeps zero visible, limits coverage claims and maps unknown diagnostics to static text', () => {
    render(<ContextSummaryCard summary={summary({ tokens: 0, calibrated: false, reportedInputTokens: null, parts: [], items: [], coverage: { complete: false, issues: ['opaque_media', '<img onerror=alert(1)>'] } })} onCategory={() => {}} onItem={() => {}} />, root);
    expect(root.textContent).toContain('0 token');
    expect(root.textContent).toContain('独立估算');
    expect(root.textContent).toContain('仅统计已分类内容');
    expect(root.textContent).toContain('媒体内容无法按文本估算');
    expect(root.textContent).not.toContain('<img');
    expect(root.textContent).not.toContain('NaN');
  });
  it('rejects unsupported versions and malformed numeric or category values', () => {
    expect(validContextPresentation(summary())).toBe(true);
    expect(validContextPresentation(summary({ tokens: -1 }))).toBe(false);
    expect(validContextPresentation({ ...summary(), schemaVersion: 2 } as unknown as ContextPresentation)).toBe(false);
    expect(validContextPresentation({ ...summary(), parts: [{ kind: 'unexpected' }] } as unknown as ContextPresentation)).toBe(false);
    expect(validContextPresentation(summary({ tokens: 0, reportedInputTokens: 0 }))).toBe(true);
  });
});
