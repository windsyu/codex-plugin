import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRun, listArtifacts, pruneArtifacts, cacheStatus, cleanCache } from '../lib/artifacts.mjs';
const DAY = 86400000;
function fixture(t) { const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'artifacts-test-'))); t.after(() => fs.rmSync(root, { recursive: true, force: true })); return root; }
function update(run, change) { const p = path.join(run.path, 'metadata.json'); const m = JSON.parse(fs.readFileSync(p)); fs.writeFileSync(p, JSON.stringify({ ...m, ...change })); }
function aged(root, days, options) { const run = createRun(root, options); run.finish(true); update(run, { createdAt: Date.now() - (days + 1) * DAY, completedAt: Date.now() - days * DAY }); return run; }
test('dry-run is zero mutation and exact expiry boundary is enforced', t => {
 const root = fixture(t), run = aged(root, 8); const meta = JSON.parse(fs.readFileSync(path.join(run.path, 'metadata.json'))); const at = meta.completedAt + 7 * DAY;
 assert.equal(pruneArtifacts(root, { now: at - 1 }).artifacts[0].eligible, false);
 const before = fs.readFileSync(path.join(run.path, 'metadata.json'), 'utf8');
 assert.equal(pruneArtifacts(root, { now: at }).artifacts[0].eligible, true);
 assert.equal(fs.readFileSync(path.join(run.path, 'metadata.json'), 'utf8'), before);
 assert.equal(pruneArtifacts(root, { apply: true, now: at }).artifacts[0].deleted, true);
});
test('failure and build artifacts require 30 days', t => {
 const root = fixture(t), failed = aged(root, 8), build = aged(root, 8, { kind: 'build' }); update(failed, { result: 'failure' });
 assert.ok(listArtifacts(root).artifacts.every(e => !e.eligible));
 assert.ok(pruneArtifacts(root, { now: Date.now() + 23 * DAY }).artifacts.every(e => e.eligible));
 assert.ok(fs.existsSync(build.path));
});
test('active, pinned, future, damaged, unknown artifacts and legacy data are protected', t => {
 const root = fixture(t); createRun(root); const pinned = aged(root, 40); update(pinned, { pinned: true });
 const future = aged(root, 40); update(future, { completedAt: Date.now() + DAY });
 const damaged = aged(root, 40); fs.writeFileSync(path.join(damaged.path, 'metadata.json'), '{');
 fs.mkdirSync(path.join(root, 'target', 'artifacts', 'unregistered')); fs.mkdirSync(path.join(root, 'target', 'observer-data'));
 const report = pruneArtifacts(root, { apply: true }); assert.equal(report.artifacts.length, 5); assert.ok(report.artifacts.every(e => !e.deleted)); assert.equal(report.unknown[0].name, 'observer-data');
});
test('symlink target and internal symlinks are rejected without following them', t => {
 const root = fixture(t), outside = fixture(t); fs.symlinkSync(outside, path.join(root, 'target'));
 assert.throws(() => createRun(root), /Symlink/); fs.unlinkSync(path.join(root, 'target'));
 const run = aged(root, 40); fs.writeFileSync(path.join(outside, 'secret'), 'keep'); fs.symlinkSync(outside, path.join(run.path, 'escape'));
 const report = pruneArtifacts(root, { apply: true }); assert.equal(report.artifacts[0].eligible, false); assert.equal(fs.readFileSync(path.join(outside, 'secret'), 'utf8'), 'keep');
});
test('lock contention refuses mutation and finish validates lease', t => {
 const root = fixture(t), run = createRun(root), lock = path.join(root, 'target', '.artifact-maintenance', 'lock'); fs.writeFileSync(lock, '{}');
 assert.throws(() => pruneArtifacts(root, { apply: true }), /lock/); fs.unlinkSync(lock);
 update(run, { lease: { token: 'changed' } }); assert.throws(() => run.finish(true), /lease/);
});
test('automatic cleanup runs at most once per day; standalone remains available', t => {
 const root = fixture(t); aged(root, 40); pruneArtifacts(root, { apply: true, automatic: true }); const next = aged(root, 40);
 assert.match(pruneArtifacts(root, { apply: true, automatic: true }).skipped, /Daily/); assert.ok(fs.existsSync(next.path));
 pruneArtifacts(root, { apply: true }); assert.equal(fs.existsSync(next.path), false);
});
test('empty reports do not create target; cache status has explicit unknown age', t => {
 const root = fixture(t); assert.deepEqual(listArtifacts(root).artifacts, []); pruneArtifacts(root); assert.equal(cacheStatus(root).profiles[0].ageDays, null); assert.equal(fs.existsSync(path.join(root, 'target')), false);
 assert.throws(() => cleanCache(root, { profile: '' }), /profiles/);
});
test('foreign repository metadata is protected', t => { const root = fixture(t), run = aged(root, 40); update(run, { repo: '/other' }); assert.equal(pruneArtifacts(root, { apply: true }).artifacts[0].eligible, false); });
test('partial deletion failure is reported while other eligible runs complete', t => {
 const root = fixture(t); const a = aged(root, 40), b = aged(root, 40); const original = fs.rmSync;
 fs.rmSync = (p, options) => { if (p === a.path) { fs.unlinkSync(path.join(p, 'metadata.json')); throw new Error('simulated partial I/O failure'); } return original(p, options); };
 let result; try { result = pruneArtifacts(root, { apply: true }); } finally { fs.rmSync = original; }
 assert.equal(result.artifacts.find(e => e.id === a.id).deleted, false); assert.match(result.artifacts.find(e => e.id === a.id).error, /partial/); assert.equal(result.artifacts.find(e => e.id === b.id).deleted, true);
 assert.equal(pruneArtifacts(root, { apply: true }).artifacts[0].eligible, false);
});
test('cache cleaning pins profile and protects unknown target directories with synthetic Cargo', t => {
 const root = fixture(t), bin = path.join(root, 'bin'); fs.mkdirSync(bin);
 fs.writeFileSync(path.join(bin, 'cargo'), `#!${process.execPath}\nconst args=process.argv.slice(2);if(args[0]==='metadata')console.log(JSON.stringify({target_directory:${JSON.stringify(path.join(root, 'target'))},build_directory:${JSON.stringify(path.join(root, 'target'))}}));else console.log(args.join(' '));\n`, { mode: 0o755 });
 fs.writeFileSync(path.join(bin, 'ps'), `#!${process.execPath}\n`, { mode: 0o755 });
 const vars = ['PATH', 'CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR']; const previous = Object.fromEntries(vars.map(k => [k, process.env[k]]));
 process.env.PATH = `${bin}${path.delimiter}${process.env.PATH}`; process.env.CARGO_HOME = path.join(root, 'cargo-home');
 for (const k of vars.slice(2)) delete process.env[k];
 try {
  fs.mkdirSync(path.join(root, 'target', 'observer-data'), { recursive: true }); fs.writeFileSync(path.join(root, 'target', 'observer-data', 'db'), 'protected');
  const preview = cleanCache(root); assert.equal(preview.stdout.trim(), `clean --profile dev --target-dir ${path.join(root, 'target')} --config build.build-dir=${JSON.stringify(path.join(root, 'target'))} --dry-run`); assert.equal(fs.existsSync(path.join(root, 'target', '.artifact-maintenance')), false);
  assert.equal(cleanCache(root, { apply: true, profile: 'release' }).stdout.trim(), `clean --profile release --target-dir ${path.join(root, 'target')} --config build.build-dir=${JSON.stringify(path.join(root, 'target'))}`); assert.equal(fs.readFileSync(path.join(root, 'target', 'observer-data', 'db'), 'utf8'), 'protected');
  assert.notEqual(cacheStatus(root).profiles[1].ageDays, null);
  const active = createRun(root); assert.throws(() => cleanCache(root), /Active artifact/); active.finish(true);
  fs.writeFileSync(path.join(bin, 'ps'), `#!${process.execPath}\nconsole.log('999 ./target/debug/codex-view');\n`, { mode: 0o755 }); assert.throws(() => cleanCache(root), /Active Cargo/);
  fs.writeFileSync(path.join(bin, 'ps'), `#!${process.execPath}\n`, { mode: 0o755 });
  fs.mkdirSync(path.join(root, 'target', 'debug')); fs.writeFileSync(path.join(bin, 'lsof'), `#!${process.execPath}\nconsole.log('999');\n`, { mode: 0o755 }); assert.throws(() => cleanCache(root), /Open cache/);
  fs.writeFileSync(path.join(bin, 'lsof'), `#!${process.execPath}\nprocess.exit(1);\n`, { mode: 0o755 }); assert.equal(cleanCache(root).status, 0);
  const cargoStub = fs.readFileSync(path.join(bin, 'cargo')); fs.writeFileSync(path.join(bin, 'cargo'), `#!${process.execPath}\nprocess.exit(2);\n`, { mode: 0o755 }); assert.throws(() => cleanCache(root), /metadata failed/); fs.writeFileSync(path.join(bin, 'cargo'), cargoStub);
  for (const buildDirectory of [undefined, '../shared', path.join(root, 'shared-cache')]) {
   fs.writeFileSync(path.join(bin, 'cargo'), `#!${process.execPath}\nconsole.log(${JSON.stringify(JSON.stringify({ target_directory: path.join(root, 'target'), build_directory: buildDirectory }))});\n`, { mode: 0o755 });
   assert.throws(() => cleanCache(root), /unknown build directory/);
  }
  fs.writeFileSync(path.join(bin, 'cargo'), cargoStub);
  fs.mkdirSync(path.join(root, '.cargo')); fs.writeFileSync(path.join(root, '.cargo', 'config.toml'), '[build]\n"build-dir" = "../shared"\n'); assert.throws(() => cleanCache(root), /Configured/);
 } finally { for (const k of vars) { if (previous[k] === undefined) delete process.env[k]; else process.env[k] = previous[k]; } }
});

test('real offline Cargo project refuses profile data deletion in preview and apply', t => {
 const root = fixture(t);
 fs.writeFileSync(path.join(root, 'Cargo.toml'), '[package]\nname = "cleanup-fixture"\nversion = "0.1.0"\nedition = "2021"\n');
 fs.mkdirSync(path.join(root, 'src')); fs.writeFileSync(path.join(root, 'src', 'main.rs'), 'fn main() {}\n');
 const generated = spawnSync('cargo', ['generate-lockfile', '--offline'], { cwd: root, encoding: 'utf8' }); assert.equal(generated.status, 0, generated.stderr);
 const profile = path.join(root, 'target', 'debug'); fs.mkdirSync(profile, { recursive: true });
 for (const name of ['observer-data/backup.db', 'deps/libmasquerade-1234567890abcdef.rlib', 'unclassified-notes.txt', 'build/some-build/out/metadata.json']) {
  const file = path.join(profile, name); fs.mkdirSync(path.dirname(file), { recursive: true }); const contents = name.includes('masquerade') ? 'SQLite format 3\0user database' : 'protected'; fs.writeFileSync(file, contents);
  for (const apply of [false, true]) assert.throws(() => cleanCache(root, { apply }), /Unclassified\/protected/);
  assert.equal(fs.readFileSync(file, 'utf8'), contents); fs.rmSync(profile, { recursive: true }); fs.mkdirSync(profile);
 }
 const active = createRun(root); update(active, { pinned: true }); assert.throws(() => cleanCache(root, { apply: true }), /unproven completion/);
 fs.writeFileSync(path.join(active.path, 'metadata.json'), '{'); assert.throws(() => cleanCache(root, { apply: true }), /unproven completion/);
});

function realCargoFixture(t) {
 const root = fixture(t), bin = path.join(root, 'bin'); fs.mkdirSync(bin);
 const vars = ['PATH', 'CARGO_HOME', 'RUSTUP_TOOLCHAIN', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR'];
 const previous = Object.fromEntries(vars.map(k => [k, process.env[k]]));
 t.after(() => { for (const k of vars) { if (previous[k] === undefined) delete process.env[k]; else process.env[k] = previous[k]; } });
 process.env.PATH = `${bin}${path.delimiter}${process.env.PATH}`; process.env.CARGO_HOME = path.join(root, 'cargo-home'); process.env.RUSTUP_TOOLCHAIN = '1.95.0';
 for (const k of vars.slice(3)) delete process.env[k];
 for (const [name, status] of [['ps', 0], ['lsof', 1]]) fs.writeFileSync(path.join(bin, name), `#!${process.execPath}\nprocess.exit(${status});\n`, { mode: 0o755 });
 fs.writeFileSync(path.join(root, 'Cargo.toml'), '[package]\nname="cleanup-fixture"\nversion="0.1.0"\nedition="2021"\n');
 fs.mkdirSync(path.join(root, 'src')); fs.writeFileSync(path.join(root, 'src/main.rs'), 'fn main() {}\n');
 assert.match(spawnSync('cargo', ['--version'], { encoding: 'utf8' }).stdout, /^cargo 1\.95\.0 /);
 const result = spawnSync('cargo', ['generate-lockfile', '--offline'], { cwd: root, encoding: 'utf8' }); assert.equal(result.status, 0, result.stderr);
 return root;
}
function snapshot(root) {
 return fs.readdirSync(root, { recursive: true }).sort().map(name => {
  const file = path.join(root, name), stat = fs.lstatSync(file);
  return [name, stat.mode, stat.mtimeMs, stat.isFile() ? fs.readFileSync(file).toString('base64') : stat.isSymbolicLink() ? fs.readlinkSync(file) : null];
 });
}
test('real Cargo rejects escaped build-directory redirects before deleting protected data', t => {
 const root = realCargoFixture(t);
 fs.mkdirSync(path.join(root, '.cargo'));
 fs.writeFileSync(path.join(root, '.cargo/config.toml'), '[build]\n"build\\u002ddir" = "shared-cache"\n');
 const file = path.join(root, 'shared-cache/debug/observer-data/history.db'); fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, 'protected');
 for (const apply of [false, true]) {
  assert.throws(() => cleanCache(root, { apply }), /build directory/i);
  assert.equal(fs.readFileSync(file, 'utf8'), 'protected');
 }
});
test('real Cargo 1.95 build can preview without mutation and clean its standard outputs', t => {
 const root = realCargoFixture(t);
 const built = spawnSync('cargo', ['build', '--offline', '--locked'], { cwd: root, encoding: 'utf8' }); assert.equal(built.status, 0, built.stderr);
 const before = snapshot(root);
 assert.equal(cleanCache(root).status, 0); assert.deepEqual(snapshot(root), before);
 const profile = path.join(root, 'target/debug');
 for (const name of ['notes.txt', 'history.db', 'deps/libfake-1234567890abcdef.rlib', 'linked-output']) {
  const file = path.join(profile, name);
  if (name === 'linked-output') fs.symlinkSync(path.join(root, 'Cargo.toml'), file);
  else fs.writeFileSync(file, name.includes('libfake') ? 'SQLite format 3\0private data' : 'preserve');
  for (const apply of [false, true]) assert.throws(() => cleanCache(root, { apply }), /Unclassified\/protected/);
  assert.ok(fs.lstatSync(file)); fs.unlinkSync(file);
 }
 const active = createRun(root);
 for (const apply of [false, true]) assert.throws(() => cleanCache(root, { apply }), /Active artifact/);
 active.finish(true);
 assert.equal(cleanCache(root, { apply: true }).status, 0);
 assert.equal(fs.existsSync(path.join(root, 'target/debug/cleanup-fixture')), false);
});
