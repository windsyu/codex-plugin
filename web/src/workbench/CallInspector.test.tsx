import { render } from 'preact';
import { afterEach, expect, it, vi } from 'vitest';
import { CallInspector, callRequestIds } from './CallInspector';
import { readSnapshot } from './reading';

const root = document.createElement('div');
const trigger = document.createElement('button');
afterEach(() => { render(null, root); root.remove(); trigger.remove(); });
it.each([false, true])('does not claim conversation purpose for unbound WebSocket creates (partiallyBound=%s)', partiallyBound => {
  const reading = readSnapshot({ runEpoch: 'ws', schemaVersion: 2, viewSeq: 1, capture: 'ok', items: [], responses: [], requests: [
    ...[1, 2].map(index => ({ requestId: 'socket', clientRequestIndex: index, responseId: partiallyBound && index === 1 ? 'response-1' : null, purpose: 'conversation', requestedModel: 'fixture', codexThreadId: 'thread', codexTurnId: `turn-${index}`, purposeBasis: 'codex_turn_metadata' })),
  ] }, 'ws');
  render(<CallInspector reading={reading} epoch="ws" requestId={null} onSelect={() => {}} onClose={() => {}} />, root);
  expect(root.querySelector('.wb-call-row')?.getAttribute('data-purpose')).toBe('unknown');
  expect(root.textContent).toContain(partiallyBound ? '多种用途' : '用途未确认');
  expect(root.textContent).toContain(partiallyBound ? 'WebSocket，部分关联未确认' : 'WebSocket，关联未确认');
  expect(root.textContent).not.toContain('各次 create 分别归属');
});
it('describes a socket containing prewarm and two bound turns without a false unknown-connection label', () => {
  const reading = readSnapshot({ runEpoch: 'ws', schemaVersion: 2, viewSeq: 1, capture: 'ok', items: [], responses: [], requests: [
    { requestId: 'socket', clientRequestIndex: 1, responseId: 'prewarm', purpose: 'auxiliary', requestedModel: 'fixture', codexThreadId: null, codexTurnId: null, purposeBasis: 'unknown_metadata' },
    ...[2, 3].map(index => ({ requestId: 'socket', clientRequestIndex: index, responseId: `response-${index}`, purpose: 'conversation', requestedModel: 'fixture', codexThreadId: 'thread', codexTurnId: `turn-${index}`, purposeBasis: 'codex_turn_metadata' })),
  ] }, 'ws');
  render(<CallInspector reading={reading} epoch="ws" requestId={null} onSelect={() => {}} onClose={() => {}} />, root);
  expect(root.querySelectorAll('.wb-call-row')).toHaveLength(1);
  expect(root.textContent).toContain('多种用途');
  expect(root.textContent).toContain('WebSocket，各次 create 分别归属');
  expect(root.textContent).not.toContain('WebSocket，关联未确认');
});
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
