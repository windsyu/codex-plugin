import test from 'node:test';
import assert from 'node:assert/strict';
import { parseArgs } from '../dev.mjs';

test('destructive options require an explicit action and supported profile', () => {
  for (const args of [
    ['cache', 'clean'], ['cache', 'clean', '--profile', ''], ['cache', 'clean', '--profile', '../'],
    ['cache', 'status', '--apply'], ['artifacts', 'list', '--apply'], ['artifacts', 'prune', '--aply'],
    ['build', '--apply'], ['cache', 'clean', '--profile', 'dev', '--apply', '--apply'],
  ]) assert.throws(() => parseArgs(args));
  assert.equal(parseArgs(['cache', 'clean', '--profile', 'dev']).apply, false);
  assert.equal(parseArgs(['artifacts', 'prune']).apply, false);
  assert.equal(parseArgs(['artifacts', 'prune', '--apply']).apply, true);
});

test('binary check cannot silently mix scopes or accept a Git option as ref', () => {
  assert.throws(() => parseArgs(['binaries', '--tree', '--all']));
  assert.throws(() => parseArgs(['binaries', '--tree', 'HEAD', '--all-branches']));
  assert.equal(parseArgs(['binaries']).ref, undefined);
  assert.equal(parseArgs(['binaries', '--tree', 'HEAD']).ref, 'HEAD');
});
