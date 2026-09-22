import { EditorState, StateEffect, StateField, type Text } from '@codemirror/state';
import { Decoration, EditorView, GutterMarker, ViewPlugin, drawSelection, gutter, highlightSpecialChars, keymap, lineNumbers, type DecorationSet, type ViewUpdate } from '@codemirror/view';
import { defaultKeymap } from '@codemirror/commands';
import { openSearchPanel, search, searchKeymap } from '@codemirror/search';

// Store only source line numbers for a diff, not another copy of every line's text.
export function diffTargets(doc: Text): Int32Array {
  const targets = new Int32Array(doc.lines);
  let next = 0, index = 0;
  for (const text of doc.iterLines()) {
    const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)/.exec(text);
    if (hunk) next = Number(hunk[1]);
    else if (next > 0 && (text.startsWith('+') || text.startsWith(' '))) targets[index] = next++;
    index++;
  }
  return targets;
}
const targetsField = StateField.define<Int32Array>({
  create: state => diffTargets(state.doc),
  update: (value, tr) => tr.docChanged ? diffTargets(tr.state.doc) : value,
});
const targetEffect = StateEffect.define<number | null>();
const targetField = StateField.define<number | null>({
  create: () => null,
  update(value, tr) {
    if (value !== null) value = tr.changes.mapPos(value);
    for (const effect of tr.effects) if (effect.is(targetEffect)) value = effect.value;
    return value;
  },
});
function lineDecorations(view: EditorView, diff: boolean): DecorationSet {
  const marks = [];
  const seen = new Set<number>();
  for (const range of view.visibleRanges) {
    for (let pos = range.from; pos <= range.to;) {
      const line = view.state.doc.lineAt(pos);
      if (!seen.has(line.number)) {
        seen.add(line.number);
        const kind = !diff ? 'context' : line.text.startsWith('@@') ? 'hunk' : line.text.startsWith('+') ? 'added' : line.text.startsWith('-') ? 'removed' : 'context';
        const target = view.state.field(targetField);
        const selected = target !== null && view.state.doc.lineAt(target).number === line.number;
        const source = diff ? view.state.field(targetsField)[line.number - 1] : line.number;
        marks.push(Decoration.line({ attributes: { class: `wb-code-line ${kind}${selected ? ' target' : ''}`, ...(source ? { 'data-line': String(source) } : {}) } }).range(line.from));
      }
      pos = line.to + 1;
    }
  }
  return Decoration.set(marks, true);
}
export interface FileViewContent { text: string; revision: string; targetLine?: number }
export interface FileView {
  update: (content: FileViewContent) => void;
  jump: (line: number, focus?: boolean) => void;
  find: () => void;
  destroy: () => void;
}
export function createFileView(parent: HTMLElement, content: FileViewContent, diff: boolean, onOpenLine: (line: number) => void, onLines: (count: number) => void): FileView {
  const paint = ViewPlugin.fromClass(class {
    decorations: DecorationSet;
    constructor(view: EditorView) { this.decorations = lineDecorations(view, diff); }
    update(update: ViewUpdate) {
      if (update.docChanged || update.viewportChanged || update.startState.field(targetField) !== update.state.field(targetField)) this.decorations = lineDecorations(update.view, diff);
    }
  }, { decorations: value => value.decorations });
  class SourceLine extends GutterMarker {
    constructor(readonly number: number) { super(); }
    eq(other: SourceLine) { return other.number === this.number; }
    toDOM() {
      const element = document.createElement(this.number ? 'button' : 'span');
      element.textContent = String(this.number || '·');
      if (this.number) {
        element.setAttribute('type', 'button');
        element.setAttribute('aria-label', `打开当前文件第 ${this.number} 行`);
        element.title = '打开当前文件对应行';
        element.onclick = () => onOpenLine(this.number);
      }
      return element;
    }
  }
  const view = new EditorView({ parent, doc: content.text, extensions: [
    EditorState.readOnly.of(true), EditorView.editable.of(false),
    EditorView.contentAttributes.of({ tabindex: '0', 'aria-label': diff ? 'Git 差异文本（只读）' : '文件文本（只读）', 'aria-readonly': 'true' }),
    EditorState.phrases.of({ Find: '文件内查找', next: '下一个', previous: '上一个', all: '全选匹配', 'match case': '区分大小写', regexp: '正则', 'by word': '全词', close: '关闭查找', 'Go to line': '跳转行号', go: '跳转' }),
    EditorView.theme({}, { dark: true }),
    targetField, diff ? targetsField : [], paint, drawSelection(), highlightSpecialChars(),
    diff ? gutter({ class: 'cm-lineNumbers',
      lineMarker: (view, line) => new SourceLine(view.state.field(targetsField)[view.state.doc.lineAt(line.from).number - 1]),
      lineMarkerChange: update => update.docChanged,
    }) : lineNumbers(),
    search({ top: true }), keymap.of([...searchKeymap, ...defaultKeymap]),
  ] });
  // CodeMirror hides decorative gutters from accessibility by default. Our
  // diff gutter contains real source-navigation buttons and must be exposed.
  if (diff) view.scrollDOM.querySelector('.cm-gutters')?.removeAttribute('aria-hidden');
  let revision = content.revision, requestedLine = content.targetLine;
  function jump(line: number, focus = false) {
    if (!Number.isSafeInteger(line) || line < 1 || line > view.state.doc.lines) return;
    const pos = view.state.doc.line(line).from;
    view.dispatch({ selection: { anchor: pos }, effects: [targetEffect.of(pos), EditorView.scrollIntoView(pos, { y: 'center' })] });
    if (focus) view.focus();
  }
  onLines(view.state.doc.lines);
  if (!diff && requestedLine) jump(requestedLine);
  return {
    jump,
    find: () => { openSearchPanel(view); },
    update(next) {
      if (revision !== next.revision) {
        // Replace just the changed span so selections and the viewport map through a refresh.
        const old = view.state.doc.toString(), text = next.text.replace(/\r\n?/g, '\n');
        let from = 0, oldEnd = old.length, newEnd = text.length;
        while (from < oldEnd && from < newEnd && old.charCodeAt(from) === text.charCodeAt(from)) from++;
        while (oldEnd > from && newEnd > from && old.charCodeAt(oldEnd - 1) === text.charCodeAt(newEnd - 1)) { oldEnd--; newEnd--; }
        if (from < oldEnd || from < newEnd) view.dispatch({ changes: { from, to: oldEnd, insert: text.slice(from, newEnd) } });
        revision = next.revision;
        onLines(view.state.doc.lines);
      }
      if (!diff && requestedLine !== next.targetLine) {
        requestedLine = next.targetLine;
        if (requestedLine) jump(requestedLine);
        else view.dispatch({ effects: targetEffect.of(null) });
      }
    },
    destroy: () => view.destroy(),
  };
}
