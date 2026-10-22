import { expect, it } from 'vitest';
import { SecureRecordScheduler } from './secureScheduler';
it('bounds terminal/control bursts while an ordinary request waits', () => {
  const scheduler = new SecureRecordScheduler();
  const terminal = {}, control = {}, http = {};
  const queue = [
    ...Array.from({ length: 20 }, (_, index) => ({ flow: terminal, priority: true, label: `terminal-${index}` })),
    ...Array.from({ length: 20 }, (_, index) => ({ flow: control, priority: true, label: `control-${index}` })),
    ...Array.from({ length: 3 }, (_, index) => ({ flow: http, priority: false, label: `http-${index}` }))
  ];
  const served = [];
  for (let index = 0; index < 15; index++) served.push(queue.splice(scheduler.choose(queue), 1)[0].label);
  expect(served.filter(value => value.startsWith('http'))).toEqual(['http-0', 'http-1', 'http-2']);
  expect(served[4]).toBe('http-0'); expect(served[9]).toBe('http-1'); expect(served[14]).toBe('http-2');
  expect(served.slice(0, 4)).toEqual(['terminal-0', 'control-0', 'terminal-1', 'control-1']);
});
it('never lets a same-flow priority End or Window overtake its queued body', () => {
  const scheduler = new SecureRecordScheduler(); const flow = {}, other = {};
  const queue = [{ flow, priority: false, label: 'body' }, { flow, priority: true, label: 'end' }, { flow: other, priority: true, label: 'control' }];
  const served = [];
  while (queue.length) served.push(queue.splice(scheduler.choose(queue), 1)[0].label);
  expect(served).toEqual(['control', 'body', 'end']);
});
it('rotates ordinary flows rather than draining one backlog first', () => {
  const scheduler = new SecureRecordScheduler(); const first = {}, second = {};
  const queue = [first, first, first, second, second].map(flow => ({ flow, priority: false }));
  const served = [];
  while (queue.length) served.push(queue.splice(scheduler.choose(queue), 1)[0].flow);
  expect(served).toEqual([first, second, first, second, first]);
});
