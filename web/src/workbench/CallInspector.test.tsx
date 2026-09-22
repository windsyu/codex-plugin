import { render } from 'preact';
import { afterEach, expect, it, vi } from 'vitest';
import { CallInspector } from './CallInspector';
import { readSnapshot } from './reading';

const root = document.createElement('div');
const trigger = document.createElement('button');
afterEach(() => { render(null, root); root.remove(); trigger.remove(); });
it('focuses a newly opened inspector before the next key and does not steal focus on updates', () => {
  document.body.append(trigger, root); trigger.focus();
  const reading = readSnapshot({ runEpoch: 'synthetic', schemaVersion: 2, viewSeq: 1, requests: [], responses: [], items: [], diagnostics: [], toolContexts: [] }, 'synthetic');
  const close = vi.fn();
  const show = (viewSeq: number) => render(<CallInspector reading={{ ...reading, viewSeq }} epoch="synthetic" requestId={null} onSelect={() => {}} onClose={close} />, root);
  // Deliberately do not flush passive effects: an immediate Escape must already
  // reach the new panel rather than the removed opener or the terminal.
  show(1);
  expect(document.activeElement).toBe(root.querySelector('[aria-label="关闭调用面板"]'));
  document.activeElement!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  expect(close).toHaveBeenCalledOnce();
  trigger.focus(); show(2);
  expect(document.activeElement).toBe(trigger);
});
