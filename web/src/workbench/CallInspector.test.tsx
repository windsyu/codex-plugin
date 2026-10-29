import { render } from 'preact';
import { afterEach, expect, it, vi } from 'vitest';
import { CallInspector, callRequestIds } from './CallInspector';
import { readSnapshot } from './reading';

const root = document.createElement('div');
const trigger = document.createElement('button');
afterEach(() => { render(null, root); root.remove(); trigger.remove(); });
it.each([false, true])('does not invent a model call for run-level direct diagnostics (historical=%s)', historical => {
  const reading = readSnapshot({ runEpoch: 'direct', schemaVersion: 2, viewSeq: 2, capture: 'unavailable', requests: [], responses: [], items: [], diagnostics: [{ requestId: '00000000-0000-0000-0000-000000000000', captureSeq: 0, code: 'model_capture_unavailable' }], toolContexts: [] }, 'direct');
  expect(callRequestIds(reading)).toEqual([]);
  render(<CallInspector reading={reading} epoch="direct" historical={historical} requestId={null} onSelect={() => {}} onClose={() => {}} />, root);
  expect(root.textContent).toContain('模型实时捕获不可用');
  expect(root.querySelector('.wb-call-row')).toBeNull();
  expect(root.textContent).not.toContain('模型未确认');
  expect(callRequestIds({ ...reading, diagnostics: [...reading.diagnostics, { requestId: 'real-request', captureSeq: 1, code: 'interrupted' }] })).toEqual(['real-request']);
});
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
