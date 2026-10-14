import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('shared asset build guard requires both entries and fresh sources for relay-only builds', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'relay-assets-guard-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const crate = path.join(root, 'crates/workbench-assets');
  const web = path.join(root, 'web');
  const executable = path.join(root, 'asset-build-guard');
  fs.mkdirSync(crate, { recursive: true });
  fs.mkdirSync(path.join(web, 'src'), { recursive: true });
  fs.mkdirSync(path.join(web, 'dist'));
  const source = fileURLToPath(new URL('../../crates/workbench-assets/build.rs', import.meta.url));
  const compiled = spawnSync('rustc', ['--edition=2024', source, '-o', executable], { encoding: 'utf8' });
  assert.equal(compiled.status, 0, compiled.stderr);
  const past = new Date(Date.now() - 10000), built = new Date(Date.now() - 5000);
  for (const input of ['index.html', 'workbench.html', 'package.json', 'package-lock.json', 'tsconfig.json', 'vite.config.ts', 'src/synthetic.ts']) {
    fs.writeFileSync(path.join(web, input), 'synthetic input');
    fs.utimesSync(path.join(web, input), past, past);
  }
  fs.utimesSync(path.join(web, 'src'), past, past);
  const run = () => spawnSync(executable, [], { cwd: crate, encoding: 'utf8' });
  assert.notEqual(run().status, 0, 'missing build must fail');
  fs.writeFileSync(path.join(web, 'dist/index.html'), 'synthetic index');
  fs.utimesSync(path.join(web, 'dist/index.html'), built, built);
  assert.notEqual(run().status, 0, 'missing workbench entry must fail');
  fs.writeFileSync(path.join(web, 'dist/workbench.html'), 'synthetic workbench');
  fs.utimesSync(path.join(web, 'dist/workbench.html'), built, built);
  assert.equal(run().status, 0, 'both fresh entries must pass');
  fs.utimesSync(path.join(web, 'workbench.html'), new Date(), new Date());
  assert.notEqual(run().status, 0, 'changed source entry must fail');
  fs.utimesSync(path.join(web, 'workbench.html'), past, past);
  fs.utimesSync(path.join(web, 'src/synthetic.ts'), new Date(), new Date());
  assert.notEqual(run().status, 0, 'changed nested source must fail');
});
