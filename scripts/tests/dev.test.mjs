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

test('component selection is explicit, validated and backwards compatible', () => {
  for (const command of ['build', 'check']) {
    assert.equal(parseArgs([command]).component, 'all');
    for (const component of ['workbench', 'relay', 'all']) assert.equal(parseArgs([command, '--component', component]).component, component);
  }
  assert.equal(parseArgs(['package']).component, 'workbench');
  assert.equal(parseArgs(['package', '--component', 'workbench']).component, 'workbench');
  for (const args of [
    ['build', '--component'], ['build', '--component', '--release'], ['build', '--component', '../relay'],
    ['check', '--component', 'Relay'], ['build', '--component', 'relay', '--component', 'all'],
    ['doctor', '--component', 'relay'], ['package', '--component', 'relay'], ['package', '--component', 'all'],
  ]) assert.throws(() => parseArgs(args));
});
