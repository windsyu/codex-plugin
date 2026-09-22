import { afterEach, expect, it, vi } from 'vitest';
import { EditorState } from '@codemirror/state';
import { EditorView } from '@codemirror/view';
import { createFileView, diffTargets, type FileView } from './fileView';

const host = document.createElement('div');
let reader: FileView | undefined;
afterEach(() => { reader?.destroy(); reader = undefined; host.remove(); host.replaceChildren(); vi.restoreAllMocks(); });
it('maps added/context lines across diff hunks without assigning removed lines', () => {
  const doc = EditorState.create({ doc: '@@ -1,2 +1,2 @@\n same\n-old\n+new\n@@ -30 +40 @@\n+tail\n\\ No newline at end of file' }).doc;
  expect([...diffTargets(doc)]).toEqual([0, 1, 0, 2, 0, 40, 0]);
});
it('keeps the same model and selection on unchanged reads and maps selection through file changes', () => {
  document.body.append(host);
  reader = createFileView(host, { text: 'first\r\nsecond\r\nlast', revision: 'one', targetLine: 2 }, false, vi.fn(), vi.fn());
  const view = EditorView.findFromDOM(host.querySelector('.cm-editor')!)!;
  const state = view.state;
  expect(state.doc.lines).toBe(3);
  expect(state.readOnly).toBe(true);
  expect(view.contentDOM.getAttribute('contenteditable')).toBe('false');
  reader.update({ text: 'first\r\nsecond\r\nlast', revision: 'one', targetLine: 2 });
  expect(view.state).toBe(state);
  reader.update({ text: 'inserted\r\nfirst\r\nsecond\r\nlast', revision: 'two', targetLine: 2 });
  expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(3);
  reader.jump(1); expect(view.state.selection.main.head).toBe(0);
  reader.jump(999); expect(view.state.selection.main.head).toBe(0);
  reader.destroy(); reader = undefined;
  expect(host.childElementCount).toBe(0);
});
it('preserves complete long lines, empty files and exposes only search in read-only mode', () => {
  document.body.append(host);
  reader = createFileView(host, { text: 'x'.repeat(16000) + 'END', revision: 'one' }, false, vi.fn(), vi.fn());
  const view = EditorView.findFromDOM(host.querySelector('.cm-editor')!)!;
  expect(view.state.doc.line(1).length).toBe(16003);
  reader.find();
  expect(host.querySelector('[name="search"]')).not.toBeNull();
  expect(host.querySelector('[name="replace"]')).toBeNull();
  reader.update({ text: '', revision: 'empty' });
  expect(view.state.doc.length).toBe(0); expect(view.state.doc.lines).toBe(1);
});

it('exposes diff source navigation to keyboard and assistive technology', () => {
  document.body.append(host);
  const open = vi.fn();
  reader = createFileView(host, { text: '@@ -1 +2 @@\n-old\n+new', revision: 'diff' }, true, open, vi.fn());
  expect(host.querySelector('.cm-gutters')?.hasAttribute('aria-hidden')).toBe(false);
  const source = host.querySelector<HTMLButtonElement>('button[aria-label="打开当前文件第 2 行"]')!;
  expect(source).not.toBeNull();
  source.click(); expect(open).toHaveBeenCalledWith(2);
});
