import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const installer = fileURLToPath(new URL('../install-macos.sh', import.meta.url));
const releaseScripts = fileURLToPath(new URL('../release/', import.meta.url));
const beginMarker = '# >>> code-view PATH >>>';
const endMarker = '# <<< code-view PATH <<<';
const resources = ['LICENSE', 'THIRD_PARTY_NOTICES.md', 'RELEASE.md', 'manifest.json', 'Cargo.lock', 'web/package-lock.json', 'CHANGELOG.md', 'vendor/vt100/LICENSE', 'vendor/vt100/PATCH.md', 'web/src/workbench/icons/LICENSE', 'web/src/workbench/icons/README.md'];
const hash = value => createHash('sha256').update(value).digest('hex');
function command(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  return result;
}
function fixture(t, { specialPrefix = false, zdot = false } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'code-view-quick-install-')));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const home = path.join(root, 'isolated home 中文');
  const tools = path.join(root, 'tools');
  const downloads = path.join(root, 'downloads');
  const sentinel = path.join(root, 'injection-sentinel');
  const prefix = specialPrefix ? path.join(root, `prefix ' quoted $(touch ${sentinel})`) : path.join(home, '.local');
  const zdotdir = zdot ? path.join(root, 'custom ZDOTDIR') : home;
  for (const directory of [home, tools, downloads, zdotdir]) fs.mkdirSync(directory, { recursive: true });
  const releaseName = 'code-view-0.3.0-aarch64-apple-darwin';
  const source = path.join(root, releaseName);
  fs.mkdirSync(path.join(source, 'bin'), { recursive: true });
  fs.copyFileSync(path.join(releaseScripts, 'install.sh'), path.join(source, 'install.sh'));
  fs.copyFileSync(path.join(releaseScripts, 'code-view'), path.join(source, 'bin/code-view'));
  fs.chmodSync(path.join(source, 'bin/code-view'), 0o755);
  for (const name of ['codex-view', 'codex-observerd']) fs.writeFileSync(path.join(source, 'bin', name), '#!/bin/sh\nprintf "%s\\n" "codex-view 0.3.0"\n', { mode: 0o755 });
  for (const name of [...resources, 'licenses/example/LICENSE']) {
    fs.mkdirSync(path.dirname(path.join(source, name)), { recursive: true });
    fs.writeFileSync(path.join(source, name), name === 'manifest.json' ? JSON.stringify({ version: '0.3.0', target: 'aarch64-apple-darwin' }) : `fixture ${name}\n`);
  }
  const packageFiles = ['install.sh', 'bin/code-view', 'bin/codex-view', 'bin/codex-observerd', ...resources, 'licenses/example/LICENSE'];
  fs.writeFileSync(path.join(source, 'CHECKSUMS.sha256'), packageFiles.map(name => `${hash(fs.readFileSync(path.join(source, name)))}  ${name}\n`).join(''));
  const archive = `${releaseName}.tar.gz`;
  const tar = command('/usr/bin/tar', ['-czf', path.join(downloads, archive), '-C', root, releaseName]);
  assert.equal(tar.status, 0, tar.stderr);
  fs.writeFileSync(path.join(downloads, 'SHA256SUMS'), `${hash(fs.readFileSync(path.join(downloads, archive)))}  ${archive}\n`);
  const log = path.join(root, 'gh-log.jsonl');
  fs.writeFileSync(path.join(tools, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo "${MOCK_SYSTEM:-Darwin}";; -m) echo "${MOCK_ARCH:-arm64}";; *) exit 1;; esac\n', { mode: 0o755 });
  const gh = `#!${process.execPath}\nconst fs=require('node:fs'),path=require('node:path');\nconst args=process.argv.slice(2);\nfs.appendFileSync(process.env.MOCK_GH_LOG,JSON.stringify(args)+'\\n');\nif(args[0]==='auth'&&args[1]==='status'){process.exit(process.env.MOCK_GH_UNAUTH==='1'?1:0)}\nif(args[0]==='release'&&args[1]==='view'){\nconst tag=process.env.MOCK_RELEASE_TAG||'v0.3.0';const data={tagName:tag,isDraft:false,isPrerelease:false,assets:fs.readdirSync(process.env.MOCK_RELEASE_DOWNLOADS).map(name=>({name}))};\nconsole.log(args.includes('--jq')||args.includes('-q')?tag:JSON.stringify(data));\nprocess.exit(0);}\nif(args[0]==='release'&&args[1]==='download'){\nif(process.env.MOCK_DOWNLOAD_EDIT)fs.appendFileSync(process.env.MOCK_DOWNLOAD_EDIT,'# concurrent binary edit\\n');\nlet directory=process.cwd();for(let i=0;i<args.length;i++){if(args[i]==='--dir'||args[i]==='-D')directory=args[++i];else if(args[i].startsWith('--dir='))directory=args[i].slice(6);}\nfs.mkdirSync(directory,{recursive:true});for(const name of fs.readdirSync(process.env.MOCK_RELEASE_DOWNLOADS))fs.copyFileSync(path.join(process.env.MOCK_RELEASE_DOWNLOADS,name),path.join(directory,name));process.exit(0);}\nconsole.error('Unexpected mock gh invocation',args);process.exit(70);\n`;
  fs.writeFileSync(path.join(tools, 'gh'), gh, { mode: 0o755 });
  const env = { ...process.env, HOME: home, USERPROFILE: home, CODEX_HOME: path.join(home, '.codex'), PATH: `${tools}:${process.env.PATH}`, MOCK_RELEASE_DOWNLOADS: downloads, MOCK_GH_LOG: log };
  delete env.ZDOTDIR;
  if (zdot) env.ZDOTDIR = zdotdir;
  const share = path.join(prefix, 'share/code-view');
  const run = (args = [], extra = {}, script = installer) => command('/bin/bash', [script, ...args], { cwd: root, env: { ...env, ...extra } });
  const install = (args = []) => run(['install', '--prefix', prefix, ...args]);
  const uninstall = (args = []) => run(['uninstall', '--prefix', prefix, ...args]);
  const calls = () => fs.existsSync(log) ? fs.readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) : [];
  const publish = version => {
    const name = `code-view-${version}-aarch64-apple-darwin`;
    const updated = path.join(root, name);
    fs.cpSync(source, updated, { recursive: true });
    for (const binary of ['codex-view', 'codex-observerd']) fs.writeFileSync(path.join(updated, 'bin', binary), `#!/bin/sh\nprintf '%s\\n' 'codex-view ${version}'\n`, { mode: 0o755 });
    fs.writeFileSync(path.join(updated, 'manifest.json'), JSON.stringify({ version, target: 'aarch64-apple-darwin' }));
    fs.writeFileSync(path.join(updated, 'CHECKSUMS.sha256'), packageFiles.map(file => `${hash(fs.readFileSync(path.join(updated, file)))}  ${file}\n`).join(''));
    for (const file of fs.readdirSync(downloads)) fs.unlinkSync(path.join(downloads, file));
    const tar = command('/usr/bin/tar', ['-czf', path.join(downloads, `${name}.tar.gz`), '-C', root, name]);
    assert.equal(tar.status, 0, tar.stderr);
    fs.writeFileSync(path.join(downloads, 'SHA256SUMS'), `${hash(fs.readFileSync(path.join(downloads, `${name}.tar.gz`)))}  ${name}.tar.gz\n`);
    env.MOCK_RELEASE_TAG = `v${version}`;
  };
  const amendPackageInstaller = text => {
    fs.writeFileSync(path.join(source, 'install.sh'), text);
    fs.writeFileSync(path.join(source, 'CHECKSUMS.sha256'), packageFiles.map(file => `${hash(fs.readFileSync(path.join(source, file)))}  ${file}\n`).join(''));
    const tar = command('/usr/bin/tar', ['-czf', path.join(downloads, archive), '-C', root, releaseName]);
    assert.equal(tar.status, 0, tar.stderr);
    fs.writeFileSync(path.join(downloads, 'SHA256SUMS'), `${hash(fs.readFileSync(path.join(downloads, archive)))}  ${archive}\n`);
  };
  const unsafeArchive = kind => {
    let args;
    if (kind === 'link') {
      fs.symlinkSync(home, path.join(source, 'linked-home'));
      args = ['-czf', path.join(downloads, archive), '-C', root, releaseName];
    } else {
      fs.writeFileSync(path.join(root, 'outside-member'), 'malicious archive fixture');
      args = ['-czf', path.join(downloads, archive), '-C', root];
      if (kind === 'traversal') args.push('-s', '|^outside-member$|../escape|');
      args.push('outside-member');
    }
    const tar = command('/usr/bin/tar', args);
    assert.equal(tar.status, 0, tar.stderr);
    fs.writeFileSync(path.join(downloads, 'SHA256SUMS'), `${hash(fs.readFileSync(path.join(downloads, archive)))}  ${archive}\n`);
  };
  return { root, home, prefix, tools, downloads, archive, sentinel, zdotdir, env, share, run, install, uninstall, calls, publish, amendPackageInstaller, unsafeArchive };
}
function succeeds(result) { assert.equal(result.status, 0, result.stderr || result.stdout); }
function fails(result) { assert.notEqual(result.status, 0, result.stdout); }
function snapshot(root) {
  const result = {};
  if (!fs.existsSync(root)) return result;
  function visit(directory, relative = '') {
    for (const name of fs.readdirSync(directory).sort()) {
      const key = path.join(relative, name);
      const file = path.join(directory, name);
      const stat = fs.lstatSync(file);
      if (stat.isSymbolicLink()) result[key] = `link:${fs.readlinkSync(file)}`;
      else if (stat.isDirectory()) { result[key] = 'directory'; visit(file, key); }
      else result[key] = hash(fs.readFileSync(file));
    }
  }
  visit(root);
  return result;
}

test('authenticated old release installs complete files, PATH, receipt and a standalone uninstaller', t => {
  const f = fixture(t);
  succeeds(f.install());
  for (const name of ['code-view', 'codex-view', 'codex-observerd', 'code-view-uninstall']) assert.equal(fs.statSync(path.join(f.prefix, 'bin', name)).mode & 0o111, 0o111);
  for (const name of resources) assert.equal(fs.readFileSync(path.join(f.share, name), 'utf8'), name === 'manifest.json' ? JSON.stringify({ version: '0.3.0', target: 'aarch64-apple-darwin' }) : `fixture ${name}\n`);
  assert.match(fs.readFileSync(path.join(f.share, '.install-receipt'), 'utf8'), /^code-view-install-v1\n/);
  for (const profile of [path.join(f.home, '.zshrc'), path.join(f.home, '.bash_profile')]) {
    const text = fs.readFileSync(profile, 'utf8');
    assert.equal(text.split(beginMarker).length - 1, 1);
    assert.equal(text.split(endMarker).length - 1, 1);
  }
  assert.ok(f.calls().some(args => args[0] === 'auth' && args[1] === 'status'));
  assert.ok(f.calls().some(args => args[0] === 'release' && args[1] === 'download'));
  for (const args of f.calls().filter(args => args[0] === 'release')) assert.ok(args.includes('windsyu/codex-plugin'), JSON.stringify(args));
  succeeds(f.run(['--prefix', f.prefix], {}, path.join(f.prefix, 'bin/code-view-uninstall')));
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view')), false);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view-uninstall')), false);
  assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
  assert.equal(fs.existsSync(path.join(f.home, '.bash_profile')), false);
});

test('uninstall preserves original shell text and unknown user files and data', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  const original = '# user setup\nexport USER_SETTING=keep\n';
  fs.writeFileSync(profile, original);
  succeeds(f.install());
  fs.writeFileSync(path.join(f.share, 'user-extra'), 'keep extra');
  fs.mkdirSync(path.join(f.home, '.codex-web/history'), { recursive: true });
  fs.writeFileSync(path.join(f.home, '.codex-web/history/user-data'), 'private');
  succeeds(f.uninstall());
  assert.equal(fs.readFileSync(profile, 'utf8'), original);
  assert.equal(fs.readFileSync(path.join(f.share, 'user-extra'), 'utf8'), 'keep extra');
  assert.equal(fs.readFileSync(path.join(f.home, '.codex-web/history/user-data'), 'utf8'), 'private');
});

test('force upgrade preserves extras and one registered PATH block', t => {
  const f = fixture(t);
  succeeds(f.install());
  fs.writeFileSync(path.join(f.share, 'user-extra'), 'keep');
  const before = fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8');
  fails(f.install());
  f.publish('0.3.1');
  succeeds(f.install(['--force', '--version', 'v0.3.1']));
  assert.match(command(path.join(f.prefix, 'bin/code-view'), ['--version'], { env: f.env }).stdout, /0\.3\.1/);
  assert.equal(fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8'), before);
  assert.equal(fs.readFileSync(path.join(f.share, 'user-extra'), 'utf8'), 'keep');
});

test('no-path creates no profiles on first install and preserves registered PATH on forced reinstall', t => {
  const f = fixture(t);
  succeeds(f.install(['--no-path']));
  assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
  assert.equal(fs.existsSync(path.join(f.home, '.bash_profile')), false);
  succeeds(f.uninstall());
  succeeds(f.install());
  const before = fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8');
  succeeds(f.install(['--force', '--no-path']));
  assert.equal(fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8'), before);
  succeeds(f.uninstall());
});

test('custom ZDOTDIR and explicit version work without touching HOME zshrc', t => {
  const f = fixture(t, { zdot: true });
  succeeds(f.install(['--version', 'v0.3.0']));
  assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
  assert.match(fs.readFileSync(path.join(f.zdotdir, '.zshrc'), 'utf8'), /code-view PATH/);
  assert.ok(f.calls().some(args => args[0] === 'release' && args.includes('v0.3.0')));
  succeeds(f.uninstall());
  assert.equal(fs.existsSync(path.join(f.zdotdir, '.zshrc')), false);
});

test('shell-special prefix is quoted literally when the managed PATH block is sourced', t => {
  const f = fixture(t, { specialPrefix: true });
  succeeds(f.install());
  const sourced = command('/bin/bash', ['--noprofile', '--norc', '-c', '. "$1"; printf "%s" "$PATH"', 'test', path.join(f.home, '.bash_profile')], { env: f.env });
  succeeds(sourced);
  assert.ok(sourced.stdout.split(':').includes(path.join(f.prefix, 'bin')));
  const zsh = command('/bin/zsh', ['-f', '-c', '. "$1"; printf "%s" "$PATH"', 'test', path.join(f.home, '.zshrc')], { env: f.env });
  succeeds(zsh);
  assert.ok(zsh.stdout.split(':').includes(path.join(f.prefix, 'bin')), 'default macOS zsh must preserve the literal prefix');
  assert.equal(fs.existsSync(f.sentinel), false);
  succeeds(f.uninstall());
});

test('modified installed file prevents every uninstall write', t => {
  const f = fixture(t);
  succeeds(f.install());
  fs.appendFileSync(path.join(f.prefix, 'bin/codex-view'), '# user modification\n');
  const before = snapshot(f.home);
  fails(f.uninstall());
  assert.deepEqual(snapshot(f.home), before);
});

test('modified PATH block or management profiles refuse uninstall before any write', t => {
  for (const relative of ['.zshrc', '.local/share/code-view/.install-path-profiles', '.local/share/code-view/.install-path-block']) {
    const f = fixture(t);
    succeeds(f.install());
    const file = path.join(f.home, relative);
    if (relative === '.zshrc') fs.writeFileSync(file, fs.readFileSync(file, 'utf8').replace(beginMarker, `${beginMarker} changed`));
    else fs.appendFileSync(file, 'modified management data\n');
    const before = snapshot(f.home);
    fails(f.uninstall());
    assert.deepEqual(snapshot(f.home), before, relative);
  }
});

test('receipt traversal and malformed header cannot remove an outside file', t => {
  for (const mutation of ['traversal', 'header']) {
    const f = fixture(t);
    succeeds(f.install(['--no-path']));
    const outside = path.join(f.home, 'outside-file');
    fs.writeFileSync(outside, 'untouched');
    const receipt = path.join(f.share, '.install-receipt');
    if (mutation === 'traversal') fs.appendFileSync(receipt, `${hash('untouched')}\t../outside-file\n`);
    else fs.writeFileSync(receipt, fs.readFileSync(receipt, 'utf8').replace('code-view-install-v1', 'unknown-format'));
    const before = snapshot(f.home);
    fails(f.uninstall());
    assert.deepEqual(snapshot(f.home), before);
    assert.equal(fs.readFileSync(outside, 'utf8'), 'untouched');
  }
});

test('receipt and managed file symbolic links refuse uninstall with zero changes', t => {
  for (const relative of ['share/code-view/.install-receipt', 'bin/codex-view']) {
    const f = fixture(t);
    succeeds(f.install(['--no-path']));
    const destination = path.join(f.prefix, relative);
    const outside = path.join(f.home, `outside-${path.basename(relative)}`);
    fs.copyFileSync(destination, outside);
    fs.unlinkSync(destination);
    fs.symlinkSync(outside, destination);
    const before = snapshot(f.home);
    fails(f.uninstall());
    assert.deepEqual(snapshot(f.home), before);
  }
});

test('extra management and profile conflicts reject installation before package files are copied', t => {
  for (const relative of ['bin/code-view-uninstall', 'share/code-view/.install-receipt', 'share/code-view/.install-path-block', 'share/code-view/.install-path-profiles']) {
    const f = fixture(t);
    const conflict = path.join(f.prefix, relative);
    fs.mkdirSync(path.dirname(conflict), { recursive: true });
    fs.writeFileSync(conflict, 'unowned user file');
    const before = snapshot(f.home);
    fails(f.install(['--force']));
    assert.deepEqual(snapshot(f.home), before, relative);
  }
  const f = fixture(t);
  const outside = path.join(f.root, 'outside-shell');
  fs.writeFileSync(outside, '# untouched\n');
  fs.symlinkSync(outside, path.join(f.home, '.zshrc'));
  const before = snapshot(f.home);
  fails(f.install());
  assert.deepEqual(snapshot(f.home), before);
  assert.equal(fs.readFileSync(outside, 'utf8'), '# untouched\n');
});

test('platform, authentication and archive checksum failures create no installation files', t => {
  for (const extra of [{ MOCK_SYSTEM: 'Linux' }, { MOCK_ARCH: 'x86_64' }, { MOCK_GH_UNAUTH: '1' }]) {
    const f = fixture(t);
    fails(f.run(['install', '--prefix', f.prefix], extra));
    assert.equal(fs.existsSync(f.prefix), false);
    assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
  }
  const f = fixture(t);
  fs.appendFileSync(path.join(f.downloads, f.archive), 'corrupt');
  fails(f.install());
  assert.equal(fs.existsSync(f.prefix), false);
});

test('uninstall restores shell profile bytes even when the original has no trailing newline', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  const original = Buffer.from('# original 中文\nexport KEEP=yes');
  fs.writeFileSync(profile, original);
  succeeds(f.install());
  succeeds(f.uninstall());
  assert.deepEqual(fs.readFileSync(profile), original);
});

test('uninstall follows registered ZDOTDIR after the environment changes', t => {
  const f = fixture(t, { zdot: true });
  const originalProfile = path.join(f.zdotdir, '.zshrc');
  fs.writeFileSync(originalProfile, '# original custom profile');
  succeeds(f.install());
  const changed = path.join(f.root, 'different ZDOTDIR');
  fs.mkdirSync(changed);
  fs.writeFileSync(path.join(changed, '.zshrc'), '# unrelated');
  succeeds(f.run(['uninstall', '--prefix', f.prefix], { ZDOTDIR: changed }));
  assert.equal(fs.readFileSync(originalProfile, 'utf8'), '# original custom profile');
  assert.equal(fs.readFileSync(path.join(changed, '.zshrc'), 'utf8'), '# unrelated');
});

test('duplicate PATH blocks reject both reinstall and uninstall before any managed write', t => {
  const f = fixture(t);
  succeeds(f.install());
  fs.appendFileSync(path.join(f.home, '.zshrc'), fs.readFileSync(path.join(f.share, '.install-path-block')));
  const before = snapshot(f.home);
  fails(f.install(['--force']));
  assert.deepEqual(snapshot(f.home), before);
  fails(f.uninstall());
  assert.deepEqual(snapshot(f.home), before);
});

test('symbolic-link ancestor of a receipt-owned license refuses all uninstall writes', t => {
  const f = fixture(t);
  succeeds(f.install(['--no-path']));
  const directory = path.join(f.share, 'licenses/example');
  const outside = path.join(f.root, 'outside-license');
  fs.renameSync(directory, outside);
  fs.symlinkSync(outside, directory);
  const before = snapshot(f.home);
  const outsideBefore = snapshot(outside);
  fails(f.uninstall());
  assert.deepEqual(snapshot(f.home), before);
  assert.deepEqual(snapshot(outside), outsideBefore);
});

test('ordinary-file removal failure retains receipt and helper, and a retry completes safely', t => {
  const f = fixture(t);
  succeeds(f.install());
  const flag = path.join(f.root, 'rm-failed-once');
  const target = path.join(f.prefix, 'bin/codex-view');
  fs.writeFileSync(path.join(f.tools, 'rm'), '#!/bin/sh\nfor arg do\n  if [ "$arg" = "$MOCK_RM_TARGET" ] && [ ! -e "$MOCK_RM_FLAG" ]; then\n    : > "$MOCK_RM_FLAG"\n    exit 73\n  fi\ndone\nexec /bin/rm "$@"\n', { mode: 0o755 });
  const extra = { MOCK_RM_TARGET: target, MOCK_RM_FLAG: flag };
  fails(f.run(['uninstall', '--prefix', f.prefix], extra));
  assert.equal(fs.existsSync(flag), true);
  assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), true);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view-uninstall')), true);
  assert.equal(fs.existsSync(target), true);
  assert.match(fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8'), /code-view PATH/);
  succeeds(f.run(['uninstall', '--prefix', f.prefix], extra));
  assert.equal(fs.existsSync(target), false);
  assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), false);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view-uninstall')), false);
  assert.equal(fs.existsSync(path.join(f.home, '.zshrc')), false);
});

test('atomic PATH write failure preserves the entire original profile and permits uninstall retry', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  const original = '# valuable existing settings\nexport KEEP_ME=yes';
  fs.writeFileSync(profile, original);
  succeeds(f.install());
  const installed = fs.readFileSync(profile);
  const flag = path.join(f.root, 'cat-failed-once');
  fs.writeFileSync(path.join(f.tools, 'cat'), '#!/bin/sh\nfor arg do\n  case "$arg" in */profile-0)\n    if [ ! -e "$MOCK_CAT_FLAG" ]; then\n      : > "$MOCK_CAT_FLAG"\n      printf "%s" "partial output"\n      exit 73\n    fi ;;\n  esac\ndone\nexec /bin/cat "$@"\n', { mode: 0o755 });
  const extra = { MOCK_CAT_FLAG: flag };
  fails(f.run(['uninstall', '--prefix', f.prefix], extra));
  assert.equal(fs.existsSync(flag), true, 'write fault must be exercised');
  assert.deepEqual(fs.readFileSync(profile), installed, 'failed write must never truncate the user profile');
  assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), true);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view-uninstall')), true);
  succeeds(f.run(['uninstall', '--prefix', f.prefix], extra));
  assert.equal(fs.readFileSync(profile, 'utf8'), original);
  assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), false);
});

test('profile edit after planning is preserved and refuses a stale PATH commit', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  const original = '# original user settings\n';
  fs.writeFileSync(profile, original);
  const packageInstaller = fs.readFileSync(path.join(releaseScripts, 'install.sh'), 'utf8');
  f.amendPackageInstaller(`${packageInstaller}\nprintf '%s\\n' '# concurrent user edit' >> "$HOME/.zshrc"\n`);
  const result = f.install();
  fails(result);
  assert.equal(fs.readFileSync(profile, 'utf8'), `${original}# concurrent user edit\n`, 'user edit between plan and commit must survive');
  assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), false);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view-uninstall')), false);
  assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view')), false);
  f.amendPackageInstaller(packageInstaller);
  succeeds(f.install());
  assert.ok(fs.readFileSync(profile, 'utf8').startsWith(`${original}# concurrent user edit\n`));
  succeeds(f.uninstall());
  assert.equal(fs.readFileSync(profile, 'utf8'), `${original}# concurrent user edit\n`);
});

function failCommandDestinationOnce(f, program, destinationPattern) {
  const flag = path.join(f.root, `${program}-failed-once`);
  const script = `#!/bin/sh\nlast=\nfor arg do last=$arg; done\ncase "$last" in ${destinationPattern})\n  if [ ! -e "$MOCK_COMMAND_FLAG" ]; then\n    : > "$MOCK_COMMAND_FLAG"\n    exit 73\n  fi ;;\nesac\nexec /bin/${program} "$@"\n`;
  fs.writeFileSync(path.join(f.tools, program), script, { mode: 0o755 });
  return { flag, extra: { MOCK_COMMAND_FLAG: flag }, remove: () => fs.unlinkSync(path.join(f.tools, program)) };
}

test('upgrade copy failure keeps all previous installed bytes and permits a clean retry', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.home, '.zshrc'), '# existing user config\n');
  succeeds(f.install());
  fs.writeFileSync(path.join(f.share, 'user-extra'), 'keep extra');
  const before = snapshot(f.home);
  f.publish('0.3.1');
  const fault = failCommandDestinationOnce(f, 'cp', '*/bin/codex-observerd');
  fails(f.run(['install', '--prefix', f.prefix, '--force', '--version', 'v0.3.1'], fault.extra));
  assert.equal(fs.existsSync(fault.flag), true, 'copy fault must be exercised');
  assert.deepEqual(snapshot(f.home), before, 'failed upgrade must retain coherent old receipt and old binaries');
  fault.remove();
  succeeds(f.install(['--force', '--version', 'v0.3.1']));
  assert.match(command(path.join(f.prefix, 'bin/code-view'), ['--version'], { env: f.env }).stdout, /0\.3\.1/);
  assert.equal(fs.readFileSync(path.join(f.share, 'user-extra'), 'utf8'), 'keep extra');
  succeeds(f.uninstall());
  assert.equal(fs.readFileSync(path.join(f.home, '.zshrc'), 'utf8'), '# existing user config\n');
});

test('initial management-file copy failure leaves no partial installation and permits a clean retry', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  fs.writeFileSync(profile, '# original settings without newline');
  const original = fs.readFileSync(profile);
  const fault = failCommandDestinationOnce(f, 'cp', '*/.install-path-profiles');
  fails(f.run(['install', '--prefix', f.prefix], fault.extra));
  assert.equal(fs.existsSync(fault.flag), true, 'management copy fault must be exercised');
  assert.deepEqual(fs.readFileSync(profile), original);
  for (const relative of ['bin/code-view', 'bin/codex-view', 'bin/codex-observerd', 'bin/code-view-uninstall', 'share/code-view/.install-receipt', 'share/code-view/.install-path-profiles']) assert.equal(fs.existsSync(path.join(f.prefix, relative)), false, relative);
  assert.equal(fs.existsSync(path.join(f.home, '.bash_profile')), false);
  fault.remove();
  succeeds(f.install());
  succeeds(f.uninstall());
  assert.deepEqual(fs.readFileSync(profile), original);
});

test('atomic binary commit failure rolls an upgrade back to the complete prior installation', t => {
  const f = fixture(t);
  succeeds(f.install());
  const before = snapshot(f.home);
  f.publish('0.3.1');
  const fault = failCommandDestinationOnce(f, 'mv', '*/bin/codex-observerd');
  fails(f.run(['install', '--prefix', f.prefix, '--force', '--version', 'v0.3.1'], fault.extra));
  assert.equal(fs.existsSync(fault.flag), true, 'atomic commit fault must be exercised');
  assert.deepEqual(snapshot(f.home), before, 'rollback must preserve original receipt, binaries and profiles');
  fault.remove();
  succeeds(f.install(['--force', '--version', 'v0.3.1']));
  assert.match(command(path.join(f.prefix, 'bin/code-view'), ['--version'], { env: f.env }).stdout, /0\.3\.1/);
  succeeds(f.uninstall());
});

test('archive outside members, traversal and symbolic links are rejected before any user writes', t => {
  for (const kind of ['outside', 'traversal', 'link']) {
    const f = fixture(t);
    fs.writeFileSync(path.join(f.home, '.zshrc'), '# original shell config');
    const before = snapshot(f.home);
    f.unsafeArchive(kind);
    fails(f.install());
    assert.deepEqual(snapshot(f.home), before, kind);
    assert.equal(fs.existsSync(path.join(f.root, 'escape')), false);
    assert.equal(fs.existsSync(path.join(f.home, 'outside-member')), false);
  }
});

test('binary edit during release download rejects force upgrade and preserves the edited old installation', t => {
  const f = fixture(t);
  succeeds(f.install());
  const binary = path.join(f.prefix, 'bin/codex-view');
  const original = fs.readFileSync(binary);
  const edit = '# concurrent binary edit\n';
  const before = snapshot(f.home);
  f.publish('0.3.1');
  const result = f.run(['install', '--prefix', f.prefix, '--force', '--no-path', '--version', 'v0.3.1'], { MOCK_DOWNLOAD_EDIT: binary });
  fails(result);
  const expected = Buffer.concat([original, Buffer.from(edit)]);
  assert.deepEqual(fs.readFileSync(binary), expected, 'download-time edit must not be overwritten');
  before['.local/bin/codex-view'] = hash(expected);
  assert.deepEqual(snapshot(f.home), before, 'all other managed bytes must remain from the old installation');
});

test('profile edit immediately before snapshot is retained through install and uninstall or a clean refusal', t => {
  const f = fixture(t);
  const profile = path.join(f.home, '.zshrc');
  const original = '# existing valuable settings\n';
  const edit = '# concurrent snapshot edit\n';
  fs.writeFileSync(profile, original);
  const flag = path.join(f.root, 'profile-snapshot-edit-once');
  fs.writeFileSync(path.join(f.tools, 'cp'), '#!/bin/sh\nlast=\nfor arg do last=$arg; done\ncase "$last" in */profile-original-0)\n  if [ ! -e "$MOCK_SNAPSHOT_FLAG" ]; then\n    : > "$MOCK_SNAPSHOT_FLAG"\n    printf "%s\\n" "# concurrent snapshot edit" >> "$MOCK_PROFILE"\n  fi ;;\nesac\nexec /bin/cp "$@"\n', { mode: 0o755 });
  const result = f.run(['install', '--prefix', f.prefix], { MOCK_SNAPSHOT_FLAG: flag, MOCK_PROFILE: profile });
  assert.equal(fs.existsSync(flag), true, 'snapshot-time edit must be exercised');
  assert.ok(fs.readFileSync(profile, 'utf8').startsWith(original + edit), 'snapshot-time user edit must survive planning and commit');
  fs.unlinkSync(path.join(f.tools, 'cp'));
  if (result.status === 0) {
    assert.match(fs.readFileSync(profile, 'utf8'), /code-view PATH/);
    succeeds(f.uninstall());
  } else {
    assert.equal(fs.existsSync(path.join(f.prefix, 'bin/code-view')), false);
    assert.equal(fs.existsSync(path.join(f.share, '.install-receipt')), false);
  }
  assert.equal(fs.readFileSync(profile, 'utf8'), original + edit);
});

test('README bootstrap stops failed downloads and temp creation, cleans up and exports PATH only on success', t => {
  const f = fixture(t);
  const readme = fs.readFileSync(new URL('../../README.md', import.meta.url), 'utf8');
  const snippet = readme.match(/```sh\n([\s\S]*?)\n```/)[1];
  fs.writeFileSync(path.join(f.tools, 'gh'), '#!/bin/sh\ncat <<\'INSTALLER\'\n#!/bin/bash\nprintf "executed\\n" > "$HOME/installer-ran"\nexit "${BOOTSTRAP_INSTALL_EXIT:-0}"\nINSTALLER\n[ "$BOOTSTRAP_MODE" != download-failure ] || exit 5\n', { mode: 0o755 });
  fs.writeFileSync(path.join(f.tools, 'mktemp'), '#!/bin/sh\n[ "$BOOTSTRAP_MODE" != mktemp-failure ] || exit 6\nexec /usr/bin/mktemp "$@"\n', { mode: 0o755 });
  const initialPath = `${f.tools}:/usr/bin:/bin`;
  for (const shell of ['/bin/bash', '/bin/zsh'].filter(shell => fs.existsSync(shell))) {
    for (const [mode, expectedExit, shouldExecute] of [['download-failure', 5, false], ['mktemp-failure', 6, false], ['installer-failure', 7, true], ['success', 0, true]]) {
      const home = path.join(f.root, `${path.basename(shell)} ${mode}`);
      const temporary = path.join(home, 'tmp');
      fs.mkdirSync(temporary, { recursive: true });
      const env = { ...f.env, HOME: home, USERPROFILE: home, CODEX_HOME: path.join(home, '.codex'), TMPDIR: temporary, PATH: initialPath, BOOTSTRAP_MODE: mode, BOOTSTRAP_INSTALL_EXIT: mode === 'installer-failure' ? '7' : '0' };
      const script = `${snippet}\nbootstrap_status=$?\nprintf '%s' "$PATH" > "$HOME/bootstrap-path"\nexit "$bootstrap_status"\n`;
      const result = command(shell, ['-f', '-c', script], { env });
      assert.equal(result.status, expectedExit, `${shell}: ${mode}: ${result.stderr}`);
      assert.equal(fs.existsSync(path.join(home, 'installer-ran')), shouldExecute, `${shell}: ${mode}`);
      assert.equal(fs.readFileSync(path.join(home, 'bootstrap-path'), 'utf8'), mode === 'success' ? `${home}/.local/bin:${initialPath}` : initialPath, `${shell}: ${mode}`);
      assert.deepEqual(fs.readdirSync(temporary), [], `${shell}: temporary script must be removed`);
    }
  }
});
