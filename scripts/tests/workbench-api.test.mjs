import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const { runApiPath } = require('../../web/e2e/workbench-api.cjs');
const run = '01234567-89ab-4def-8123-456789abcdef';
test('browser probes keep Run API scope after legacy URLs become document routes', () => {
  const expected = `/workbench/v1/runs/${run}/run`;
  assert.equal(runApiPath(`http://localhost/?run=${run}`, '/run'), expected);
  for (const page of ['chat', 'terminal', 'files', 'search', 'git', 'history', 'usage', 'settings', 'access']) {
    assert.equal(runApiPath(`http://localhost/runs/${run}/${page}`, '/run'), expected);
  }
  assert.equal(runApiPath('http://localhost/', '/run'), '/workbench/v1/run');
  assert.throws(() => runApiPath(`http://localhost/runs/${run}/unknown`, '/run'));
  assert.throws(() => runApiPath('http://localhost/runs/not-a-run/chat', '/run'));
  assert.throws(() => runApiPath(`http://localhost/runs/${run}/chat?run=11111111-1111-4111-8111-111111111111`, '/run'));
});
