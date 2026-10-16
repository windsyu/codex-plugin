import test from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { assertComponentBoundaries, buildComponent, cargoScope } from '../lib/components.mjs';

test('independent builds compile frontend first and select only the requested product', async () => {
  for (const [component, pkg] of [['workbench', 'codex-local-observer'], ['relay', 'codex-relayd']]) {
    const calls = [];
    await buildComponent(async (...call) => calls.push(call), { component, locked: ['--locked', '--offline'], release: true });
    assert.deepEqual(calls, [
      ['npm', ['run', 'build', '--prefix', 'web']],
      ['cargo', ['build', '--locked', '--offline', '-p', pkg, '--bins', '--release']],
    ]);
  }
});

test('component checks include shared contracts, full checks explicitly include workspace', async () => {
  for (const component of ['workbench', 'relay', 'all']) {
    const calls = [];
    await buildComponent(async (...call) => calls.push(call), { component, check: true, locked: ['--locked'] });
    const scope = cargoScope(component), tests = cargoScope(component, { tests: true });
    assert.deepEqual(calls, [
      ['npm', ['run', 'build', '--prefix', 'web']],
      ['cargo', ['fmt', '--all', '--check']],
      ['cargo', ['clippy', '--locked', ...tests, '--all-targets', '--', '-D', 'warnings']],
      ['cargo', ['test', '--locked', ...tests, '--all-targets', '--', '--test-threads=4']],
      ['cargo', ['build', '--locked', ...scope, '--bins']],
      ['cargo', ['build', '--locked', ...scope, '--release', '--bins']],
    ]);
    if (component === 'all') assert.deepEqual(scope, ['--workspace']);
    else assert.deepEqual(tests.slice(2), ['-p', 'codex-relay-protocol', '-p', 'codex-workbench-assets']);
  }
});

test('workbench packaging recipe uses only local binaries and explicit target', async () => {
  const calls = [];
  await buildComponent(async (...call) => calls.push(call), {
    component: 'workbench', locked: ['--locked'], release: true, target: 'aarch64-apple-darwin',
  });
  assert.deepEqual(calls[1], ['cargo', ['build', '--locked', '-p', 'codex-local-observer', '--bins', '--release', '--target', 'aarch64-apple-darwin']]);
});

test('failed frontend build never starts Cargo and invalid scope never starts frontend', async () => {
  const calls = [];
  await assert.rejects(buildComponent(async (...call) => { calls.push(call); throw new Error('frontend failed'); }, {
    component: 'relay', locked: ['--locked'],
  }), /frontend failed/);
  assert.equal(calls.length, 1);
  await assert.rejects(buildComponent(async (...call) => calls.push(call), { component: 'relai', locked: ['--locked'] }), /Unknown component/);
  assert.equal(calls.length, 1);
});

test('real Cargo workspace preserves product and shared-package production boundaries', () => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const result = spawnSync('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const metadata = JSON.parse(result.stdout);
  assertComponentBoundaries(metadata);
  const local = metadata.packages.find(pkg => pkg.name === 'codex-local-observer');
  assert.deepEqual(metadata.workspace_default_members, [local.id]);
  assert.equal(path.resolve(local.manifest_path), path.join(root, 'Cargo.toml'));
  // Catch a future direct/transitive production coupling, while keeping dev fixtures.
  const altered = structuredClone(metadata);
  altered.packages.find(pkg => pkg.name === 'codex-relayd').dependencies.push({ name: 'codex-local-observer', path: root, kind: 'build' });
  assert.throws(() => assertComponentBoundaries(altered), /crosses deployment boundary/);
  const shared = structuredClone(metadata);
  shared.packages.find(pkg => pkg.name === 'codex-workbench-assets').dependencies.push({ name: 'codex-relayd', path: path.join(root, 'relay'), kind: null });
  assert.throws(() => assertComponentBoundaries(shared), /crosses deployment boundary/);
});
