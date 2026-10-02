import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const scripts = fileURLToPath(new URL('../release/', import.meta.url));
const resources = ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'RELEASE.md', 'manifest.json', 'Cargo.lock', 'web/package-lock.json', 'CHANGELOG.md', 'vendor/vt100/LICENSE', 'vendor/vt100/PATCH.md', 'web/src/workbench/icons/LICENSE', 'web/src/workbench/icons/README.md'];
function fixture(t) {
  const temporary = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'code-view-release-')));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const release = path.join(temporary, 'release 包 with spaces');
  const home = path.join(temporary, 'home');
  const project = path.join(temporary, 'project 中文 with spaces');
  const tools = path.join(temporary, 'tools');
  for (const directory of [release, home, project, tools, path.join(release, 'bin'), path.join(release, 'licenses', 'nested')]) fs.mkdirSync(directory, { recursive: true });
  fs.copyFileSync(path.join(scripts, 'code-view'), path.join(release, 'bin/code-view'));
  fs.copyFileSync(path.join(scripts, 'install.sh'), path.join(release, 'install.sh'));
  fs.chmodSync(path.join(release, 'bin/code-view'), 0o755);
  const backend = `#!${process.execPath}\nrequire('node:fs').writeFileSync(process.env.CAPTURE, JSON.stringify({cwd:process.cwd(), argv:process.argv.slice(2)}));\n`;
  for (const name of ['codex-view', 'codex-observerd']) fs.writeFileSync(path.join(release, 'bin', name), backend, { mode: 0o755 });
  for (const name of [...resources, 'licenses/nested/LICENSE']) {
    fs.mkdirSync(path.dirname(path.join(release, name)), { recursive: true });
    fs.writeFileSync(path.join(release, name), name);
  }
  const checksumFiles = ['bin/code-view', 'bin/codex-view', 'bin/codex-observerd', 'install.sh', ...resources, 'licenses/nested/LICENSE'];
  fs.writeFileSync(path.join(release, 'CHECKSUMS.sha256'), checksumFiles.map(name => `${createHash('sha256').update(fs.readFileSync(path.join(release, name))).digest('hex')}  ${name}\n`).join(''));
  const capture = path.join(temporary, 'capture.json');
  const env = { ...process.env, HOME: home, USERPROFILE: home, CODEX_HOME: path.join(home, '.codex'), CAPTURE: capture };
  const uname = (system = 'Darwin', arch = 'arm64') => fs.writeFileSync(path.join(tools, 'uname'), `#!/bin/sh\ncase "$1" in -s) echo '${system}';; -m) echo '${arch}';; *) exit 1;; esac\n`, { mode: 0o755 });
  const install = (args = []) => spawnSync('/bin/sh', [path.join(release, 'install.sh'), ...args], { env: { ...env, PATH: `${tools}:${process.env.PATH}` }, cwd: project, encoding: 'utf8' });
  const launch = (args, executable = path.join(release, 'bin/code-view')) => {
    const result = spawnSync(executable, args, { cwd: project, env, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return { ...JSON.parse(fs.readFileSync(capture, 'utf8')), stdout: result.stdout };
  };
  uname();
  return { temporary, release, home, project, tools, env, uname, install, launch };
}

test('launcher preserves cwd and passes arguments exactly', t => {
  const f = fixture(t);
  const cases = [
    [[], ['--project', '.']],
    [['--no-open', '--profile', 'a b'], ['--project', '.', '--no-open', '--profile', 'a b']],
    [['other 中文', '--profile', "a ' quoted"], ['--project', 'other 中文', '--profile', "a ' quoted"]],
    [['--history', '--no-open'], ['--no-open']],
    [['open', '/tmp/private entry'], ['open', '/tmp/private entry']],
    [['--version'], ['--version']],
    [['-V'], ['-V']],
    [['version'], ['--version']],
    [['--no-open', '--project', 'explicit'], ['--no-open', '--project', 'explicit']],
    [['--project=explicit'], ['--project=explicit']],
  ];
  for (const [args, expected] of cases) {
    const result = f.launch(args);
    assert.equal(result.cwd, f.project);
    assert.deepEqual(result.argv, expected);
  }
  for (const flag of ['help', '--help', '-h']) {
    const result = f.launch([flag]);
    assert.deepEqual(result.argv, ['--help']);
    assert.match(result.stdout, /Usage: code-view/);
    assert.match(result.stdout, /--history/);
  }
});

test('installation uses isolated default prefix, installs resources and supports explicit reinstall', t => {
  const f = fixture(t);
  const prefix = path.join(f.home, '.local');
  let result = f.install();
  assert.equal(result.status, 0, result.stderr);
  assert.doesNotMatch(result.stdout, /: OK/);
  for (const name of ['code-view', 'codex-view', 'codex-observerd']) assert.equal(fs.statSync(path.join(prefix, 'bin', name)).mode & 0o777, 0o755);
  for (const name of resources) assert.equal(fs.readFileSync(path.join(prefix, 'share/code-view', name), 'utf8'), name);
  assert.equal(fs.readFileSync(path.join(prefix, 'share/code-view/licenses/nested/LICENSE'), 'utf8'), 'licenses/nested/LICENSE');
  assert.deepEqual(f.launch([], path.join(prefix, 'bin/code-view')).argv, ['--project', '.']);
  fs.writeFileSync(path.join(prefix, 'share/code-view/extra'), 'keep');
  result = f.install();
  assert.notEqual(result.status, 0);
  result = f.install(['--force']);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(fs.readFileSync(path.join(prefix, 'share/code-view/extra'), 'utf8'), 'keep');
  assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
});

test('conflict preflight makes no partial install, then explicit force succeeds', t => {
  const f = fixture(t);
  const prefix = path.join(f.temporary, 'install 中文 spaces');
  fs.mkdirSync(path.join(prefix, 'bin'), { recursive: true });
  fs.writeFileSync(path.join(prefix, 'bin/codex-observerd'), 'existing');
  let result = f.install(['--prefix', prefix]);
  assert.notEqual(result.status, 0);
  assert.equal(fs.existsSync(path.join(prefix, 'bin/code-view')), false);
  assert.equal(fs.existsSync(path.join(prefix, 'share')), false);
  assert.equal(fs.readFileSync(path.join(prefix, 'bin/codex-observerd'), 'utf8'), 'existing');
  result = f.install(['--prefix', prefix, '--force']);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(f.launch(['--history'], path.join(prefix, 'bin/code-view')).argv, []);
});

test('installer rejects unsupported platform and invalid/missing sources before writing', t => {
  const f = fixture(t);
  const prefix = path.join(f.temporary, 'destination');
  for (const [system, arch] of [['Linux', 'aarch64'], ['Darwin', 'x86_64']]) {
    f.uname(system, arch);
    assert.notEqual(f.install(['--prefix', prefix]).status, 0);
    assert.equal(fs.existsSync(prefix), false);
  }
  f.uname();
  assert.notEqual(f.install(['--prefix', 'relative']).status, 0);
  assert.notEqual(f.install(['--prefix']).status, 0);
  assert.notEqual(f.install(['--unknown']).status, 0);
  fs.unlinkSync(path.join(f.release, 'manifest.json'));
  assert.notEqual(f.install(['--prefix', prefix]).status, 0);
  assert.equal(fs.existsSync(prefix), false);
});

test('installer refuses destination, ancestor and nested resource symbolic links even with force', t => {
  const f = fixture(t);
  const outside = path.join(f.temporary, 'outside');
  fs.mkdirSync(outside);
  fs.writeFileSync(path.join(outside, 'sentinel'), 'untouched');
  const linked = path.join(f.temporary, 'linked');
  fs.symlinkSync(outside, linked);
  assert.notEqual(f.install(['--prefix', path.join(linked, 'child'), '--force']).status, 0);
  assert.equal(fs.existsSync(path.join(outside, 'child')), false);
  const prefix = path.join(f.temporary, 'destination');
  fs.mkdirSync(path.join(prefix, 'bin'), { recursive: true });
  fs.symlinkSync(path.join(outside, 'sentinel'), path.join(prefix, 'bin/codex-view'));
  assert.notEqual(f.install(['--prefix', prefix, '--force']).status, 0);
  assert.equal(fs.existsSync(path.join(prefix, 'bin/code-view')), false);
  fs.unlinkSync(path.join(prefix, 'bin/codex-view'));
  fs.mkdirSync(path.join(prefix, 'share/code-view/licenses'), { recursive: true });
  fs.symlinkSync(outside, path.join(prefix, 'share/code-view/licenses/nested'));
  assert.notEqual(f.install(['--prefix', prefix, '--force']).status, 0);
  assert.equal(fs.existsSync(path.join(prefix, 'bin/code-view')), false);
  assert.equal(fs.readFileSync(path.join(outside, 'sentinel'), 'utf8'), 'untouched');
});

test('recursive license file/directory conflicts are rejected before writing binaries', t => {
  const f = fixture(t);
  const prefix = path.join(f.temporary, 'destination');
  fs.mkdirSync(path.join(prefix, 'share/code-view/licenses'), { recursive: true });
  fs.writeFileSync(path.join(prefix, 'share/code-view/licenses/nested'), 'existing file');
  const result = f.install(['--prefix', prefix, '--force']);
  assert.notEqual(result.status, 0, result.stderr);
  assert.equal(fs.existsSync(path.join(prefix, 'bin')), false);
});

test('checksum mismatch stops installation before creating the prefix', t => {
  const f = fixture(t);
  const prefix = path.join(f.temporary, 'destination');
  fs.appendFileSync(path.join(f.release, 'bin/codex-view'), '\n# changed\n');
  const result = f.install(['--prefix', prefix]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /checksum verification failed/);
  assert.equal(fs.existsSync(prefix), false);
});

test('every new release resource is required before installation writes', t => {
  const f = fixture(t);
  const prefix = path.join(f.temporary, 'destination');
  for (const name of resources) {
    const source = path.join(f.release, name);
    fs.renameSync(source, `${source}.saved`);
    assert.notEqual(f.install(['--prefix', prefix]).status, 0, name);
    assert.equal(fs.existsSync(prefix), false, name);
    fs.renameSync(`${source}.saved`, source);
  }
});

test('new resource destination ancestors and file type conflicts are rejected during preflight', t => {
  const f = fixture(t);
  const prefixes = ['vendor', 'vendor/vt100', 'web', 'web/src', 'web/src/workbench', 'web/src/workbench/icons'];
  const outside = path.join(f.temporary, 'outside');
  fs.mkdirSync(outside);
  let attempt = 0;
  for (const relative of [...prefixes, ...resources]) {
    for (const kind of ['wrong-type', 'symlink']) {
      const prefix = path.join(f.temporary, `conflict-${attempt++}`);
      const destination = path.join(prefix, 'share/code-view', relative);
      fs.mkdirSync(path.dirname(destination), { recursive: true });
      if (kind === 'symlink') fs.symlinkSync(outside, destination);
      else if (prefixes.includes(relative)) fs.writeFileSync(destination, 'existing file');
      else fs.mkdirSync(destination);
      const result = f.install(['--prefix', prefix, '--force']);
      assert.notEqual(result.status, 0, `${relative} ${kind}`);
      assert.equal(fs.existsSync(path.join(prefix, 'bin')), false, `${relative} ${kind}`);
    }
  }
  assert.deepEqual(fs.readdirSync(outside), []);
});
