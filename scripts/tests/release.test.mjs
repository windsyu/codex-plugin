import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { collectLicenseFiles, writeLicenses, writeChecksums, packageRelease, RELEASE_TARGET } from '../lib/release.mjs';
import { parseArgs } from '../dev.mjs';

function fixture(t) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'release-notices-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const cargo = path.join(root, 'crate');
  const npm = path.join(root, 'web/node_modules/runtime');
  const output = path.join(root, 'output');
  for (const dir of [cargo, npm, output]) fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(cargo, 'LICENSE-MIT'), 'upstream copyright\n');
  fs.mkdirSync(path.join(cargo, 'native'));
  fs.writeFileSync(path.join(cargo, 'native/NOTICE'), 'native attribution\n');
  fs.writeFileSync(path.join(npm, 'LICENSE'), 'web copyright\n');
  fs.writeFileSync(path.join(npm, 'package.json'), JSON.stringify({ name: 'runtime', version: '1.0.0', license: 'MIT' }));
  fs.writeFileSync(path.join(root, 'web/package-lock.json'), JSON.stringify({ packages: { '': {}, 'node_modules/runtime': { version: '1.0.0', license: 'MIT' }, 'node_modules/dev-tool': { dev: true }, 'node_modules/other-platform': { optional: true } } }));
  const metadata = { resolve: { root: 'root' }, packages: [{ id: 'root' }, { id: 'crate', name: 'crate', version: '1.2.3', license: 'MIT', manifest_path: path.join(cargo, 'Cargo.toml') }] };
  return { root, cargo, npm, output, metadata };
}

function packagingFixture(t, host = RELEASE_TARGET) {
  const f = fixture(t);
  const tools = path.join(f.root, 'tools');
  const home = path.join(f.root, 'home');
  const target = path.join(f.root, 'target');
  for (const directory of [tools, home, path.join(home, '.cargo')]) fs.mkdirSync(directory);
  const environment = {
    HOME: home, USERPROFILE: home, CARGO_HOME: path.join(home, '.cargo'),
    CODEX_HOME: path.join(home, '.codex'), PATH: `${tools}${path.delimiter}${process.env.PATH}`,
  };
  const previous = Object.fromEntries(Object.keys(environment).map(key => [key, process.env[key]]));
  Object.assign(process.env, environment);
  t.after(() => {
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[key]; else process.env[key] = value;
    }
  });
  const mock = (name, body) => fs.writeFileSync(path.join(tools, name), `#!${process.execPath}\n${body}\n`, { mode: 0o755 });
  mock('rustc', `console.log(${JSON.stringify(`rustc 1.95.0\nhost: ${host}`)});`);
  mock('cargo', `require('node:assert/strict').deepEqual(process.argv.slice(2), ['metadata', '--locked', '--offline', '--filter-platform', ${JSON.stringify(RELEASE_TARGET)}, '--format-version', '1']); console.log(${JSON.stringify(JSON.stringify({ ...f.metadata, target_directory: target }))});`);
  mock('otool', `console.log(process.argv[3] + ':\\n\\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)');`);
  mock('git', "if (process.argv[2] === 'rev-parse') console.log('0123456789012345678901234567890123456789'); else if (process.argv[2] !== 'status') process.exit(1);");
  return { ...f, target };
}

test('packaging selects freshly built explicit-target binaries over stale same-version host binaries', { skip: process.platform !== 'darwin' }, t => {
  const f = packagingFixture(t);
  fs.writeFileSync(path.join(f.root, 'Cargo.toml'), '[package]\nname = "release-fixture"\nversion = "0.3.0"\n');
  fs.writeFileSync(path.join(f.root, 'web/package.json'), JSON.stringify({ version: '0.3.0' }));
  const resources = ['scripts/release/code-view', 'scripts/release/install.sh', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'CHANGELOG.md', 'Cargo.lock', 'vendor/vt100/LICENSE', 'vendor/vt100/PATCH.md', 'web/src/workbench/icons/LICENSE', 'web/src/workbench/icons/README.md'];
  for (const file of resources) {
    fs.mkdirSync(path.dirname(path.join(f.root, file)), { recursive: true });
    fs.writeFileSync(path.join(f.root, file), `synthetic ${file}\n`);
  }
  for (const [directory, marker] of [[path.join(f.target, 'release'), 'stale-host-build'], [path.join(f.target, RELEASE_TARGET, 'release'), 'fresh-explicit-target-build']]) {
    fs.mkdirSync(directory, { recursive: true });
    for (const binary of ['codex-view', 'codex-observerd']) {
      fs.writeFileSync(path.join(directory, binary), `#!/bin/sh\n# ${marker}\nprintf '%s\\n' '${binary} 0.3.0'\n`, { mode: 0o755 });
    }
  }
  const release = packageRelease(f.root, f.output, { offline: true });
  for (const binary of ['codex-view', 'codex-observerd']) {
    const packaged = fs.readFileSync(path.join(release.bundle, 'bin', binary), 'utf8');
    assert.match(packaged, /fresh-explicit-target-build/);
    assert.doesNotMatch(packaged, /stale-host-build/);
  }
  assert.equal(release.target, RELEASE_TARGET);
  assert.equal(fs.statSync(release.archive).isFile(), true);
});

test('unsupported Rust host fails before package inputs are read or artifacts written', t => {
  const f = packagingFixture(t, 'x86_64-apple-darwin');
  assert.throws(() => packageRelease(f.root, f.output, { offline: true }), /supports only tested aarch64-apple-darwin; got x86_64-apple-darwin/);
  assert.deepEqual(fs.readdirSync(f.output), []);
});

test('package command only accepts applicable flags', () => {
  assert.equal(parseArgs(['package', '--offline']).offline, true);
  for (const args of [['package', '--release'], ['package', '--apply'], ['package', '--target', 'linux']]) assert.throws(() => parseArgs(args));
});
test('notices preserve nested upstream attribution and exclude unshipped web tools', t => {
  const f = fixture(t);
  assert.equal(writeLicenses(f.root, f.metadata, f.output), 2);
  const index = JSON.parse(fs.readFileSync(path.join(f.output, 'licenses/INDEX.json')));
  assert.equal(index.length, 2);
  assert.equal(fs.readFileSync(path.join(f.output, 'licenses/cargo/crate@1.2.3/native/NOTICE'), 'utf8'), 'native attribution\n');
  assert.equal(JSON.stringify(index).includes(f.root), false);
});
test('missing notice or mismatched installed dependency fails packaging', t => {
  const f = fixture(t);
  fs.unlinkSync(path.join(f.npm, 'LICENSE'));
  assert.throws(() => writeLicenses(f.root, f.metadata, f.output), /Missing license/);
  fs.rmSync(f.output, { recursive: true }); fs.mkdirSync(f.output);
  fs.writeFileSync(path.join(f.npm, 'LICENSE'), 'copyright');
  fs.writeFileSync(path.join(f.npm, 'package.json'), JSON.stringify({ name: 'runtime', version: '2.0.0' }));
  assert.throws(() => writeLicenses(f.root, f.metadata, f.output), /lock mismatch/);
});
test('notice inputs reject links and paths outside the dependency', t => {
  const f = fixture(t);
  fs.symlinkSync(f.npm, path.join(f.cargo, 'linked'));
  assert.throws(() => collectLicenseFiles(f.cargo), /Symlink/);
  fs.unlinkSync(path.join(f.cargo, 'linked'));
  f.metadata.packages[1].license_file = '../outside';
  assert.throws(() => writeLicenses(f.root, f.metadata, f.output), /escapes/);
});
test('checksums cover nested content and change when payload changes', t => {
  const f = fixture(t);
  fs.mkdirSync(path.join(f.output, 'bin'));
  const binary = path.join(f.output, 'bin/code-view');
  fs.writeFileSync(binary, 'payload');
  writeChecksums(f.output);
  const digest = crypto.createHash('sha256').update('payload').digest('hex');
  assert.equal(fs.readFileSync(path.join(f.output, 'CHECKSUMS.sha256'), 'utf8'), `${digest}  bin/code-view\n`);
  fs.writeFileSync(binary, 'changed'); writeChecksums(f.output);
  assert.equal(fs.readFileSync(path.join(f.output, 'CHECKSUMS.sha256'), 'utf8').includes(digest), false);
});
