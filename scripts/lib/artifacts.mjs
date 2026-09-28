import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';

const DAY = 86400000;
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
function layout(repoRoot) {
  const repo = fs.realpathSync(repoRoot);
  const target = path.join(repo, 'target');
  assertSafe(repo, target);
  return { repo, target, artifacts: path.join(target, 'artifacts'), state: path.join(target, '.artifact-maintenance') };
}
function assertSafe(root, candidate) {
  const relative = path.relative(root, candidate);
  if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error('Path escapes repository');
  let current = root;
  for (const part of relative.split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    if (fs.existsSync(current) || isLink(current)) {
      if (fs.lstatSync(current).isSymbolicLink()) throw new Error(`Symlink rejected: ${current}`);
    }
  }
}
function isLink(p) { try { return fs.lstatSync(p).isSymbolicLink(); } catch { return false; } }
function read(p) { return JSON.parse(fs.readFileSync(p, 'utf8')); }
function write(p, value) {
  if (isLink(p)) throw new Error('Symlink metadata rejected');
  const temporary = `${p}.${crypto.randomUUID()}.tmp`;
  try { fs.writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx', mode: 0o600 }); fs.renameSync(temporary, p); }
  finally { if (fs.existsSync(temporary)) fs.unlinkSync(temporary); }
}
function inspect(p) {
  const stat = fs.lstatSync(p);
  if (stat.isSymbolicLink()) throw new Error('Symlink in artifact');
  if (!stat.isDirectory()) return stat.size;
  return fs.readdirSync(p).reduce((size, name) => size + inspect(path.join(p, name)), 0);
}
function locked(l, action) {
  assertSafe(l.repo, l.state);
  fs.mkdirSync(l.state, { recursive: true, mode: 0o700 });
  const lock = path.join(l.state, 'lock');
  let fd;
  try { fd = fs.openSync(lock, 'wx'); } catch (error) { if (error.code === 'EEXIST') throw new Error('Maintenance lock exists; inspect owner before removing it'); throw error; }
  try { fs.writeFileSync(fd, JSON.stringify({ pid: process.pid, createdAt: Date.now() })); return action(); }
  finally { fs.closeSync(fd); fs.unlinkSync(lock); }
}
function entries(l, now) {
  if (!Number.isFinite(now) || now < 0) throw new Error('Invalid current time');
  assertSafe(l.repo, l.artifacts);
  if (!fs.existsSync(l.artifacts)) return [];
  return fs.readdirSync(l.artifacts).sort().map(id => {
    const entry = { id, path: path.join(l.artifacts, id), eligible: false, completed: false, bytes: null };
    try {
      if (!fs.lstatSync(entry.path).isDirectory() || isLink(entry.path)) throw new Error('Artifact must be a regular directory');
      if (!UUID.test(id)) throw new Error('Unregistered artifact');
      const metaPath = path.join(entry.path, 'metadata.json');
      if (!fs.lstatSync(metaPath).isFile() || isLink(metaPath)) throw new Error('Metadata must be a regular file');
      const m = read(metaPath);
      if (m.version !== 1 || m.id !== id || m.repo !== l.repo || !['task', 'build'].includes(m.kind) || typeof m.pinned !== 'boolean' || !Number.isFinite(m.createdAt) || m.createdAt < 0 || m.createdAt > now) throw new Error('Invalid or future metadata');
      entry.bytes = inspect(entry.path);
      entry.kind = m.kind;
      if (m.lease !== null || m.completedAt === null) throw new Error('Active or incomplete');
      if (!Number.isFinite(m.completedAt) || m.completedAt < m.createdAt || m.completedAt > now || !['success', 'failure'].includes(m.result)) throw new Error('Invalid or future completion');
      entry.completed = true;
      if (m.pinned) throw new Error('Pinned');
      entry.completedAt = m.completedAt;
      entry.expiresAt = m.completedAt + (m.kind === 'build' || m.result === 'failure' ? 30 : 7) * DAY;
      entry.eligible = now >= entry.expiresAt;
      entry.reason = entry.eligible ? 'Expired' : 'Within retention';
    } catch (error) { entry.reason = error.message; }
    return entry;
  });
}
export function createRun(repoRoot, { kind = 'task', label = '' } = {}) {
  if (!['task', 'build'].includes(kind)) throw new Error('Invalid artifact kind');
  const l = layout(repoRoot);
  return locked(l, () => {
    assertSafe(l.repo, l.artifacts);
    fs.mkdirSync(l.artifacts, { recursive: true, mode: 0o700 });
    const id = crypto.randomUUID();
    const runPath = path.join(l.artifacts, id);
    fs.mkdirSync(runPath, { mode: 0o700 });
    const metadata = { version: 1, id, repo: l.repo, kind, label, createdAt: Date.now(), completedAt: null, result: null, pinned: false, lease: { pid: process.pid, token: crypto.randomUUID() } };
    write(path.join(runPath, 'metadata.json'), metadata);
    const baseline = path.join(l.state, 'cache.json');
    assertSafe(l.repo, baseline);
    if (!fs.existsSync(baseline)) write(baseline, { registeredAt: Date.now(), cleanedAt: {} });
    return { id, path: runPath, finish(success) {
      return locked(l, () => {
        assertSafe(l.repo, runPath);
        assertSafe(l.repo, path.join(runPath, 'metadata.json'));
        const current = read(path.join(runPath, 'metadata.json'));
        if (current.lease?.token !== metadata.lease.token) throw new Error('Artifact lease changed');
        write(path.join(runPath, 'metadata.json'), { ...current, lease: null, completedAt: Date.now(), result: success ? 'success' : 'failure' });
      });
    } };
  });
}
export function listArtifacts(repoRoot, { now = Date.now() } = {}) {
  const l = layout(repoRoot);
  const unknown = fs.existsSync(l.target) ? fs.readdirSync(l.target).filter(n => !['artifacts', '.artifact-maintenance'].includes(n)).sort().map(name => {
    // Classification is only a hint for the owner, never permission to delete.
    const entry = { name, category: 'unclassified', policy: 'Unregistered; inventory only' };
    const p = path.join(l.target, name);
    try {
      const stat = fs.lstatSync(p);
      if (stat.isSymbolicLink()) entry.category = 'symlink-protected';
      else if (['debug', 'release'].includes(name)) entry.category = 'cargo-profile-manual-review';
      else {
        const names = stat.isDirectory() ? [name, ...fs.readdirSync(p)] : [name];
        if (names.some(n => /(?:backup|\.bundle$|branch-retirement)/i.test(n))) entry.category = 'backup-protected';
        else if (names.some(n => /(?:observer-data|codex-home|\.sqlite(?:[-.]|$)|\.db(?:[-.]|$))/i.test(n))) entry.category = 'runtime-data-protected';
        else if (stat.isFile() && /\.(?:log|stderr)$/.test(name)) entry.category = 'diagnostic-unregistered';
        else if (stat.isFile() && /\.json$/.test(name)) entry.category = 'metadata-unregistered';
      }
    } catch { entry.category = 'unreadable-protected'; }
    return entry;
  }) : [];
  return { repo: l.repo, artifacts: entries(l, now), unknown };
}
export function pruneArtifacts(repoRoot, { apply = false, automatic = false, now = Date.now() } = {}) {
  if (!Number.isFinite(now) || now < 0) throw new Error('Invalid current time');
  const l = layout(repoRoot);
  if (!apply) return { apply: false, ...listArtifacts(repoRoot, { now }) };
  return locked(l, () => {
    const stamp = path.join(l.state, 'prune.json');
    assertSafe(l.repo, stamp);
    if (automatic && fs.existsSync(stamp)) {
      const last = read(stamp).at;
      if (!Number.isFinite(last) || now - last < DAY) return { apply: true, skipped: 'Daily cleanup already attempted or stamp invalid' };
    }
    if (automatic) write(stamp, { at: now });
    const report = listArtifacts(repoRoot, { now });
    for (const entry of report.artifacts) {
      if (!entry.eligible) continue;
      try {
        assertSafe(l.repo, entry.path);
        const refreshed = entries(l, now).find(item => item.id === entry.id);
        if (!refreshed?.eligible) { entry.eligible = false; entry.reason = refreshed?.reason ?? 'Artifact disappeared'; continue; }
        inspect(entry.path);
        fs.rmSync(entry.path, { recursive: true });
        entry.deleted = true;
      } catch (error) { entry.deleted = false; entry.error = error.message; }
    }
    return { apply: true, ...report };
  });
}
export function cacheStatus(repoRoot, { now = Date.now() } = {}) {
  const l = layout(repoRoot);
  let state = {};
  const statePath = path.join(l.state, 'cache.json');
  assertSafe(l.repo, statePath);
  try { state = read(statePath); } catch { /* Unknown age is explicit. */ }
  return { sizeBasis: 'Sum of file lengths, not allocated disk blocks; hard links may be counted more than once', profiles: ['debug', 'release'].map(profile => {
    const p = path.join(l.target, profile);
    let bytes = null, error;
    try { bytes = fs.existsSync(p) ? inspect(p) : 0; } catch (e) { error = e.message; }
    const since = state.cleanedAt?.[profile] ?? state.registeredAt;
    return { profile, bytes, ageDays: Number.isFinite(since) ? (now - since) / DAY : null, suggested: bytes > 20 * 1024 ** 3 || (Number.isFinite(since) && now - since >= 30 * DAY), ...(error ? { error } : {}) };
  }) };
}
// Cargo clean removes entire profile directories, including unrelated data. Unknown
// outputs require manual classification; a Cargo-looking directory is not provenance.
function validateCacheContents(cache, metadata) {
  const targets = new Set((metadata.packages ?? []).flatMap(p => p.targets ?? []).map(t => t.name.replaceAll('-', '_')));
  function visit(dir, section = '') {
    for (const name of fs.readdirSync(dir)) {
      const file = path.join(dir, name), relative = path.relative(cache, file), stat = fs.lstatSync(file);
      const reject = () => { throw new Error(`Unclassified/protected cache content: ${relative}; review manually`); };
      if (stat.isSymbolicLink() || (!stat.isDirectory() && !stat.isFile())) reject();
      if (/(?:observer-data|backup|bundle|metadata\.json|\.(?:db|sqlite[0-9]*|bak|zip|tar|gz)(?:[-.]|$))/i.test(name)) reject();
      if (stat.isDirectory()) {
        if (!section && !['deps', '.fingerprint', 'incremental', 'build', 'examples'].includes(name)) reject();
        if (section === 'deps' || section === 'examples') reject();
        visit(file, section || name);
        continue;
      }
      const fd = fs.openSync(file, 'r');
      const header = Buffer.alloc(32);
      try { fs.readSync(fd, header, 0, header.length, 0); } finally { fs.closeSync(fd); }
      if (header.subarray(0, 16).toString() === 'SQLite format 3\0' || header.toString().startsWith('# v2 git bundle') || header.toString().startsWith('# v3 git bundle') || header.subarray(0, 4).equals(Buffer.from([0x50, 0x4b, 3, 4]))) reject();
      let known = false;
      if (!section) known = name === '.cargo-lock' || targets.has(name.replace(/\.(?:d|exe|rlib|rmeta|dylib|so|dll)$/, '').replace(/^lib/, '').replaceAll('-', '_'));
      else if (section === 'deps' || section === 'examples') known = /^(?:lib)?[A-Za-z0-9_-]+-[a-f0-9]{8,}(?:\.(?:d|rlib|rmeta|o|so|dylib|dll|a|lib|pdb|exe)|\.[a-z0-9]+(?:\.[a-z0-9]+)?\.rcgu\.o)?$/.test(name);
      else if (section === '.fingerprint') known = /^(?:invoked\.timestamp|(?:dep-|bin-|lib-|test-|run-build-script-|build-script-)[A-Za-z0-9_.-]+)$/.test(name);
      else if (section === 'incremental') known = /^(?:[a-z0-9]+\.o|dep-graph\.bin|query-cache\.bin|work-products\.bin|.*\.lock)$/.test(name);
      else if (section === 'build') known = ['output', 'stderr', 'root-output', 'invoked.timestamp', 'build-script-build'].includes(name) || /^build_script_build-[a-f0-9]+(?:\.d)?$/.test(name);
      if (!known) reject();
    }
  }
  visit(cache);
}
export function cleanCache(repoRoot, { profile = 'dev', apply = false } = {}) {
  if (!['dev', 'release'].includes(profile)) throw new Error('Only dev/release cache profiles are supported');
  const l = layout(repoRoot);
  // Explicit overrides keep Cargo's deletion roots identical to the checked roots,
  // even if configuration changes after metadata was read.
  const command = ['clean', '--profile', profile, '--target-dir', l.target, '--config', `build.build-dir=${JSON.stringify(l.target)}`, ...(apply ? [] : ['--dry-run'])];
  // Cargo configuration may redirect build artifacts independently of metadata.target_directory.
  if (process.env.CARGO_TARGET_DIR || process.env.CARGO_BUILD_TARGET_DIR || process.env.CARGO_BUILD_BUILD_DIR) throw new Error('External/configured Cargo target or build directory must be reviewed manually');
  const home = process.env.CARGO_HOME || path.join(process.env.HOME || '', '.cargo');
  const configDirs = [home];
  for (let dir = l.repo; ; dir = path.dirname(dir)) { configDirs.push(path.join(dir, '.cargo')); if (path.dirname(dir) === dir) break; }
  for (const dir of configDirs) for (const filename of ['config', 'config.toml']) {
    const p = path.join(dir, filename);
    if (fs.existsSync(p) && /(?:target-dir|build-dir|include)/.test(fs.readFileSync(p, 'utf8'))) throw new Error('Configured target/build directory requires manual cache review');
  }
  const metadata = spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1', '--locked'], { cwd: l.repo, encoding: 'utf8' });
  if (metadata.status !== 0) throw new Error(`Cargo metadata failed: ${metadata.stderr}`);
  const cargoMetadata = JSON.parse(metadata.stdout);
  for (const [field, label] of [['target_directory', 'target directory'], ['build_directory', 'build directory']]) {
    const directory = cargoMetadata[field];
    if (typeof directory !== 'string' || !path.isAbsolute(directory) || path.resolve(directory) !== l.target) throw new Error(`Shared/external or unknown ${label} rejected`);
    assertSafe(l.repo, directory);
  }
  const execute = () => {
    if (entries(l, Date.now()).some(e => !e.completed)) throw new Error('Active artifact or unproven completion blocks cache cleaning');
    const cache = path.join(l.target, profile === 'dev' ? 'debug' : 'release');
    assertSafe(l.repo, cache);
    if (fs.existsSync(cache)) validateCacheContents(cache, cargoMetadata);
    const processes = spawnSync('ps', ['-axo', 'pid=,command='], { encoding: 'utf8' });
    if (processes.status !== 0) throw new Error('Cannot establish whether builds or applications are active');
    const active = processes.stdout.split('\n').filter(line => !line.trim().startsWith(`${process.pid} `)).some(line => /(?:^|[\s/])(?:cargo|rustc|rustdoc|codex-web|codex-view|observerd|codex-local-gateway)(?:\s|$)/.test(line) || line.includes(`${l.target}/`));
    if (active) throw new Error('Active Cargo/build/workbench process blocks cache cleaning');
    if (fs.existsSync(cache)) {
      inspect(cache);
      const openFiles = spawnSync('lsof', ['-t', '+D', cache], { encoding: 'utf8' });
      if (openFiles.error || ![0, 1].includes(openFiles.status) || openFiles.stderr?.trim()) throw new Error('Cannot verify cache open-file usage');
      if (openFiles.stdout?.trim() || openFiles.status === 0) throw new Error('Open cache files block cleaning');
    }
    const result = spawnSync('cargo', command, { cwd: l.repo, encoding: 'utf8' });
    if (apply && result.status === 0) {
      const p = path.join(l.state, 'cache.json');
      assertSafe(l.repo, p);
      let state; try { state = read(p); } catch { state = { registeredAt: Date.now(), cleanedAt: {} }; }
      state.cleanedAt ??= {};
      state.cleanedAt[profile === 'dev' ? 'debug' : 'release'] = Date.now();
      write(p, state);
    }
    return { apply, command: ['cargo', ...command], status: result.status, stdout: result.stdout, stderr: result.stderr, error: result.error?.message };
  };
  return apply ? locked(l, execute) : execute();
}
