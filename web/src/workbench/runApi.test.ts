import { beforeEach, expect, it } from 'vitest';
import { runApi } from './runApi';

const originalHref = location.href;
const firstRun = '11111111-1111-4111-8111-111111111111';
const secondRun = '22222222-2222-4222-8222-222222222222';

beforeEach(() => { history.replaceState(null, '', originalHref.split('?')[0]); });

it('isolates endpoints for different run IDs', () => {
  history.replaceState(null, '', `/?run=${firstRun}`);
  expect(runApi('/live/snapshot')).toBe(`/workbench/v1/runs/${firstRun}/live/snapshot`);
  history.replaceState(null, '', `/?run=${secondRun}`);
  expect(runApi('/live/snapshot')).toBe(`/workbench/v1/runs/${secondRun}/live/snapshot`);
});

it('keeps the standalone endpoint when no run is selected', () => {
  expect(runApi('/history')).toBe('/workbench/v1/history');
});

it.each(['', 'not-a-uuid', '11111111-1111-4111-8111-11111111111z'])('rejects malformed run query %j', run => {
  history.replaceState(null, '', `/?run=${run}`);
  expect(() => runApi('/run')).toThrow('invalid run id');
});
