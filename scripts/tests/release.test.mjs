import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { collectLicenseFiles, writeLicenses, writeChecksums, packageRelease, macosRustRuntimeLicenses, RELEASE_TARGET } from '../lib/release.mjs';
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
  const sysroot = path.join(f.root, 'actual rust sysroot');
  const noticeFiles = [
    'share/doc/rust/COPYRIGHT-library.html',
    ...['MIT', 'Apache-2.0', 'Unicode-3.0', 'BSD-2-Clause', 'LLVM-exception'].map(name => `share/doc/rust/licenses/${name}.txt`),
    'lib/rustlib/src/rust/library/compiler-builtins/LICENSE.txt',
    'lib/rustlib/src/rust/library/compiler-builtins/libm/LICENSE.txt',
  ];
  for (const file of noticeFiles) {
    fs.mkdirSync(path.dirname(path.join(sysroot, file)), { recursive: true });
    fs.writeFileSync(path.join(sysroot, file), file.endsWith('.html')
      ? '<h1>Copyright notices for The Rust Standard Library</h1>\n'
      : file.endsWith('LICENSE.txt') ? 'MIT AND Apache-2.0 WITH LLVM-exception\n' : `original ${file}\n`);
  }
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
  mock('rustc', `const assert = require('node:assert/strict');
    if (process.argv[2] === '-vV') console.log(${JSON.stringify(`rustc 1.95.0\nhost: ${host}\nrelease: 1.95.0`)});
    else { assert.deepEqual(process.argv.slice(2), ['--print', 'sysroot']); console.log(${JSON.stringify(sysroot)}); }`);
  mock('cargo', `require('node:assert/strict').deepEqual(process.argv.slice(2), ['metadata', '--locked', '--offline', '--filter-platform', ${JSON.stringify(RELEASE_TARGET)}, '--format-version', '1']); console.log(${JSON.stringify(JSON.stringify({ ...f.metadata, target_directory: target }))});`);
  mock('otool', `console.log(process.argv[3] + ':\\n\\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)');`);
  mock('git', "if (process.argv[2] === 'rev-parse') console.log('0123456789012345678901234567890123456789'); else if (process.argv[2] !== 'status') process.exit(1);");
  return { ...f, target, sysroot, noticeFiles };
}

for (const version of ['0.3.0', '1.0.0', '12.34.56']) test(`packaging ${version} selects freshly built explicit-target binaries over stale same-version host binaries`, { skip: process.platform !== 'darwin' }, t => {
  const f = packagingFixture(t);
  fs.writeFileSync(path.join(f.root, 'Cargo.toml'), `[package]\nname = "release-fixture"\nversion = "${version}"\n`);
  fs.writeFileSync(path.join(f.root, 'web/package.json'), JSON.stringify({ version }));
  const resources = ['scripts/release/code-view', 'scripts/release/install.sh', 'LICENSE', 'THIRD_PARTY_NOTICES.md', 'CHANGELOG.md', 'Cargo.lock', 'vendor/vt100/LICENSE', 'vendor/vt100/PATCH.md', 'web/src/workbench/icons/LICENSE', 'web/src/workbench/icons/README.md'];
  for (const file of resources) {
    fs.mkdirSync(path.dirname(path.join(f.root, file)), { recursive: true });
    fs.writeFileSync(path.join(f.root, file), `synthetic ${file}\n`);
  }
  for (const [directory, marker] of [[path.join(f.target, 'release'), 'stale-host-build'], [path.join(f.target, RELEASE_TARGET, 'release'), 'fresh-explicit-target-build']]) {
    fs.mkdirSync(directory, { recursive: true });
    for (const binary of ['codex-view', 'codex-observerd']) {
      fs.writeFileSync(path.join(directory, binary), `#!/bin/sh\n# ${marker}\nprintf '%s\\n' '${binary} ${version}'\n`, { mode: 0o755 });
    }
  }
  const release = packageRelease(f.root, f.output, { offline: true });
  for (const binary of ['codex-view', 'codex-observerd']) {
    const packaged = fs.readFileSync(path.join(release.bundle, 'bin', binary), 'utf8');
    assert.match(packaged, /fresh-explicit-target-build/);
    assert.doesNotMatch(packaged, /stale-host-build/);
  }
  assert.equal(release.target, RELEASE_TARGET);
  assert.equal(release.version, version);
  assert.equal(JSON.parse(fs.readFileSync(path.join(release.bundle, 'manifest.json'))).version, version);
  assert.equal(path.basename(release.archive), `code-view-${version}-${RELEASE_TARGET}.tar.gz`);
  assert.equal(fs.statSync(release.archive).isFile(), true);
  const notes = fs.readFileSync(path.join(release.bundle, 'RELEASE.md'), 'utf8');
  assert.match(notes, /workbench UI\/API 3, wire transport 3 and relay schema 3/);
  assert.match(notes, /separate Application and per-Run channels/);
  assert.match(notes, /frontend and authorization service remain trusted/);
  assert.match(notes, /no plaintext fallback/);
  assert.match(notes, /Upgrade all three components together/);
  assert.match(notes, /CLI 0\.162\.1 command authentication was tested with a synthetic provider; real-provider command authentication remains unverified/);
  assert.match(notes, /Default OpenAI\/ChatGPT launches use native direct mode/);
  assert.match(notes, /live model capture and call inspection are unavailable/);
  assert.doesNotMatch(notes, /UI\/API 2|transport 2|relay is trusted and can read forwarded content/);
  const index = JSON.parse(fs.readFileSync(path.join(release.bundle, 'licenses/INDEX.json')));
  const runtimes = index.filter(entry => entry.ecosystem === 'toolchain-runtime');
  assert.deepEqual(runtimes.map(entry => [entry.name, entry.version]), [['rust-library', '1.95.0'], ['rust-compiler-builtins', '1.95.0']]);
  assert.equal(JSON.stringify(index).includes(f.sysroot), false);
  const checksums = fs.readFileSync(path.join(release.bundle, 'CHECKSUMS.sha256'), 'utf8');
  for (const entry of runtimes) {
    for (const file of entry.licenseFiles) {
      const original = file.slice(`licenses/toolchain-runtime/${entry.name}@${entry.version}/`.length);
      const payload = fs.readFileSync(path.join(release.bundle, file));
      assert.deepEqual(payload, fs.readFileSync(path.join(f.sysroot, original)));
      assert.ok(checksums.includes(`${crypto.createHash('sha256').update(payload).digest('hex')}  ${file}\n`));
    }
  }
  assert.equal(runtimes.flatMap(entry => entry.licenseFiles).length, f.noticeFiles.length);
});

test('macOS runtime notice discovery fails closed for missing, empty, binary and changed notices', async t => {
  for (const change of ['missing', 'empty', 'binary', 'unrecognized', 'symlink']) {
    await t.test(change, t => {
      const f = packagingFixture(t);
      const notice = path.join(f.sysroot, 'lib/rustlib/src/rust/library/compiler-builtins/LICENSE.txt');
      if (change === 'missing') fs.unlinkSync(notice);
      if (change === 'empty') fs.writeFileSync(notice, ' \n');
      if (change === 'binary') fs.writeFileSync(notice, Buffer.from([65, 0, 66]));
      if (change === 'unrecognized') fs.writeFileSync(notice, 'new license terms require review');
      if (change === 'symlink') { fs.unlinkSync(notice); fs.symlinkSync(path.join(f.sysroot, 'share/doc/rust/licenses/MIT.txt'), notice); }
      assert.throws(() => macosRustRuntimeLicenses(f.root), undefined, change);
      assert.deepEqual(fs.readdirSync(f.output), []);
    });
  }
});

test('runtime notice discovery rejects symlink ancestors and unknown compiler versions', t => {
  const f = packagingFixture(t);
  const library = path.join(f.sysroot, 'lib');
  const moved = path.join(f.sysroot, 'real-lib');
  fs.renameSync(library, moved);
  fs.symlinkSync(moved, library);
  assert.throws(() => macosRustRuntimeLicenses(f.root), /Symlink/);
  fs.writeFileSync(path.join(f.root, 'tools/rustc'), `#!${process.execPath}\nconsole.log('release: unknown');\n`);
  assert.throws(() => macosRustRuntimeLicenses(f.root), /stable Rust runtime version/);
});

test('packaging rejects malformed and non-stable release versions before artifacts are written', t => {
  const f = packagingFixture(t);
  for (const version of ['01.0.0', '1.00.0', '1.0.00', 'v1.0.0', '1.0', '1.0.0-beta.1', '1.0.0+build.1', '../1.0.0', '1.0.0\n']) {
    fs.writeFileSync(path.join(f.root, 'Cargo.toml'), `[package]\nversion = "${version}"\n`);
    assert.throws(() => packageRelease(f.root, f.output, { offline: true }), /Expected stable semver release version/, version);
    assert.deepEqual(fs.readdirSync(f.output), [], version);
  }
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
function runtimeNotice(f, overrides = {}) {
  const directory = path.join(f.root, 'runtime-source');
  fs.mkdirSync(path.join(directory, 'licenses'), { recursive: true });
  fs.writeFileSync(path.join(directory, 'COPYRIGHT-library.html'), 'synthetic runtime attribution\n');
  fs.writeFileSync(path.join(directory, 'licenses/MIT.txt'), 'synthetic runtime permission\n');
  return { name: 'rust-library', version: '1.95.0', declaredLicense: 'MIT OR Apache-2.0', directory,
    noticeFiles: ['COPYRIGHT-library.html', 'licenses/MIT.txt'], ...overrides };
}
test('explicit static runtime notices are indexed verbatim with no local source paths', t => {
  const f = fixture(t);
  const runtime = runtimeNotice(f);
  assert.equal(writeLicenses(f.root, f.metadata, f.output, { runtimeLicenses: [runtime] }), 3);
  const index = JSON.parse(fs.readFileSync(path.join(f.output, 'licenses/INDEX.json')));
  const entry = index.find(item => item.ecosystem === 'toolchain-runtime');
  assert.equal(entry.name, 'rust-library');
  assert.equal(entry.version, '1.95.0');
  assert.equal(JSON.stringify(index).includes(f.root), false);
  assert.equal(entry.licenseFiles.length, 2);
  for (const file of runtime.noticeFiles) {
    const output = `licenses/toolchain-runtime/rust-library@1.95.0/${file}`;
    assert.ok(entry.licenseFiles.includes(output));
    assert.deepEqual(fs.readFileSync(path.join(f.output, output)), fs.readFileSync(path.join(runtime.directory, file)));
  }
  writeChecksums(f.output);
  assert.match(fs.readFileSync(path.join(f.output, 'CHECKSUMS.sha256'), 'utf8'), /toolchain-runtime\/rust-library@1\.95\.0\/COPYRIGHT-library\.html/);
});
test('explicit runtime inputs reject missing notices, output traversal and source links', t => {
  for (const overrides of [{ noticeFiles: [] }, { noticeFiles: ['../outside'] }, { noticeFiles: ['licenses/missing.txt'] }, { name: '../escape' }, { version: '../escape' }, { declaredLicense: '' }]) {
    const f = fixture(t);
    assert.throws(() => writeLicenses(f.root, f.metadata, f.output, { runtimeLicenses: [runtimeNotice(f, overrides)] }));
  }
  const f = fixture(t);
  const runtime = runtimeNotice(f);
  fs.symlinkSync(f.npm, path.join(runtime.directory, 'linked'));
  runtime.noticeFiles = ['linked/LICENSE'];
  assert.throws(() => writeLicenses(f.root, f.metadata, f.output, { runtimeLicenses: [runtime] }), /Symlink/);
});
test('explicit runtime notices reject binary content and duplicate sources', t => {
  const f = fixture(t);
  const runtime = runtimeNotice(f);
  fs.writeFileSync(path.join(runtime.directory, 'COPYRIGHT-library.html'), Buffer.from([65, 0, 66]));
  assert.throws(() => writeLicenses(f.root, f.metadata, f.output, { runtimeLicenses: [runtime] }), /Non-text license/);
  const other = fixture(t);
  const duplicate = runtimeNotice(other, { noticeFiles: ['COPYRIGHT-library.html', 'COPYRIGHT-library.html'] });
  assert.throws(() => writeLicenses(other.root, other.metadata, other.output, { runtimeLicenses: [duplicate] }), /Duplicate/);
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
