#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const help = `Usage: node scripts/dev.mjs <command>

  doctor                              Inspect tools without changing files
  bootstrap [--offline]               Install locked web dependencies (npm ci)
  build [--release] [--offline]        Build web, then both Rust binaries
  package [--offline]                  Build and package tested macOS arm64 release
  check [--ci] [--offline]             Docs, binary policy, tool/web/Rust checks
  test-tools                          Test maintenance tools with synthetic data
  docs                                Check local Markdown links and structure
  e2e                                 Run browser tests using system Chrome
  binaries [--tree REF|--all-branches]  Check staged Git blobs by default
  artifacts list                      Inventory managed and unknown artifacts
  artifacts prune [--apply]            Preview, or delete expired managed runs
  cache status                        Report cache size and age
  cache clean --profile dev|release [--apply]
                                      Preview, or clean one idle Cargo profile

No command pushes, rewrites history, or downloads a browser. --offline is optional;
use bootstrap on a connected machine first. Full native/large-data tests are opt-in.
`;

// Reject misspelled/extra arguments before creating a task directory or deleting anything.
export function parseArgs(args) {
  const [command = 'help', ...rest] = args;
  if (['help', '--help', '-h'].includes(command)) {
    if (rest.length) throw new Error('Unexpected help arguments');
    return { command: 'help' };
  }
  const options = { command, apply: false, offline: false, release: false, ci: false };
  const flags = new Set();
  if (['artifacts', 'cache'].includes(command)) options.action = rest.shift();
  for (let i = 0; i < rest.length; i++) {
    const flag = rest[i];
    if (flags.has(flag)) throw new Error(`Repeated option: ${flag}`);
    flags.add(flag);
    if (flag === '--profile' || flag === '--tree') {
      const value = rest[++i];
      if (!value || value.startsWith('-')) throw new Error(`${flag} needs a value`);
      options[flag === '--profile' ? 'profile' : 'ref'] = value;
    } else if (flag === '--all-branches') options.history = true;
    else if (['--apply', '--offline', '--release', '--ci'].includes(flag)) options[flag.slice(2)] = true;
    else throw new Error(`Unknown option: ${flag}`);
  }
  const allowed = {
    doctor: [], bootstrap: ['--offline'], build: ['--offline', '--release'], package: ['--offline'],
    check: ['--offline', '--ci'], 'test-tools': [], docs: [], e2e: [],
    binaries: ['--tree', '--all-branches'], artifacts: ['--apply'], cache: ['--profile', '--apply'],
  }[command];
  if (!allowed) throw new Error(`Unknown command: ${command}`);
  for (const flag of flags) if (!allowed.includes(flag)) throw new Error(`${command} does not accept ${flag}`);
  if (command === 'binaries' && options.ref && options.history) throw new Error('Choose --tree or --all-branches');
  if (command === 'artifacts' && (!['list', 'prune'].includes(options.action) || (options.action === 'list' && options.apply))) throw new Error('Use artifacts list or artifacts prune [--apply]');
  if (command === 'cache') {
    if (!['status', 'clean'].includes(options.action)) throw new Error('Use cache status or cache clean');
    if (options.action === 'status' && flags.size) throw new Error('cache status takes no options');
    if (options.action === 'clean' && !['dev', 'release'].includes(options.profile)) throw new Error('cache clean requires --profile dev|release');
  }
  return options;
}

function printReport(report) {
  console.log(JSON.stringify(report, null, 2));
  if (report.errors > 0 || (report.status !== undefined && report.status !== 0)
    || report.artifacts?.some(entry => entry.error)) throw new Error('See the failed checks above');
}

function inspectTools() {
  const wantedNode = fs.readFileSync(path.join(root, '.nvmrc'), 'utf8').trim();
  const rust = fs.readFileSync(path.join(root, 'rust-toolchain.toml'), 'utf8').match(/channel\s*=\s*"([^"]+)"/)[1];
  const checks = [{ name: 'node', actual: process.versions.node, expected: wantedNode, ok: process.versions.node === wantedNode }];
  for (const [name, args, expected] of [['rustc', ['--version'], `rustc ${rust} `], ['cargo', ['--version']], ['npm', ['--version']], ['git', ['--version']]]) {
    const result = spawnSync(name, args, { cwd: root, encoding: 'utf8' });
    const actual = result.stdout?.trim();
    checks.push({ name, actual, ...(expected ? { expected: expected.trim() } : {}), ok: result.status === 0 && (!expected || actual.startsWith(expected)) });
  }
  if (process.platform === 'darwin') {
    const sdk = spawnSync('xcrun', ['--show-sdk-path'], { encoding: 'utf8' });
    checks.push({ name: 'Apple SDK', ok: sdk.status === 0 });
  }
  return { checks, errors: checks.filter(check => !check.ok).length, note: 'Chrome and official CLI are only needed by their opt-in tests; this is not an MSRV claim.' };
}

async function managed(options, action) {
  const artifacts = await import('./lib/artifacts.mjs');
  if (['build', 'check'].includes(options.command)) {
    try {
      const report = artifacts.pruneArtifacts(root, { apply: true, automatic: true });
      const deleted = report.artifacts?.filter(entry => entry.deleted).length || 0;
      console.log(`Artifact maintenance: ${report.skipped || `${deleted} expired run(s) removed`}`);
      for (const entry of report.artifacts || []) if (entry.error) console.warn(`Artifact ${entry.id}: ${entry.error}`);
    } catch (error) { console.warn(`Artifact cleanup skipped: ${error.message}`); }
  }
  const run = artifacts.createRun(root, { label: options.command, kind: options.command === 'package' ? 'build' : 'task' });
  console.log(`Task artifacts: ${run.path}`);
  let success = false, interrupted = false, child, forceStop, step = 0;
  function stop(signal) {
    interrupted = true;
    if (!child?.pid) return;
    const send = value => { try { process.kill(process.platform === 'win32' ? child.pid : -child.pid, value); } catch (error) { if (error.code !== 'ESRCH') console.warn(error.message); } };
    send(signal);
    forceStop = setTimeout(() => send('SIGKILL'), 5000);
    forceStop.unref();
  }
  const interrupt = () => stop('SIGINT'), terminate = () => stop('SIGTERM');
  process.on('SIGINT', interrupt); process.on('SIGTERM', terminate);
  const execute = async (program, args, env = {}) => {
    if (interrupted) throw new Error('Task interrupted; artifact lease retained for inspection');
    console.log(`> ${program} ${args.join(' ')}`);
    const fd = fs.openSync(path.join(run.path, `${String(++step).padStart(2, '0')}-${path.basename(program)}.log`), 'wx', 0o600);
    try {
      await new Promise((resolve, reject) => {
        child = spawn(program, args, { cwd: root, env: { ...process.env, ...env }, detached: process.platform !== 'win32', stdio: ['inherit', 'pipe', 'pipe'] });
        child.stdout.on('data', data => { process.stdout.write(data); fs.writeSync(fd, data); });
        child.stderr.on('data', data => { process.stderr.write(data); fs.writeSync(fd, data); });
        child.once('error', reject);
        child.once('close', (code, signal) => {
          child = undefined;
          clearTimeout(forceStop);
          if (code === 0 && !interrupted) resolve();
          else reject(new Error(`${program} failed (${signal || code}); inspect ${run.path}`));
        });
      });
    } finally { fs.closeSync(fd); }
  };
  try {
    await action(execute, run);
    if (interrupted) throw new Error('Task interrupted');
    success = true;
  } finally {
    process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', terminate);
    if (!interrupted) run.finish(success);
    else console.warn('Interrupted run remains protected; inspect it before manually retiring its lease.');
  }
  if (['build', 'check'].includes(options.command)) {
    for (const status of artifacts.cacheStatus(root).profiles) {
      if (status.suggested) console.warn(`Cargo ${status.profile} cache merits review (${status.bytes ?? 'unknown'} bytes, ${status.ageDays ?? 'unknown'} days). Use cache status; cleaning is explicit.`);
    }
  }
}

export async function main(args = process.argv.slice(2)) {
  const options = parseArgs(args);
  if (options.command === 'help') { console.log(help); return; }
  if (options.command === 'doctor') { printReport(inspectTools()); return; }
  if (options.command === 'docs' || options.command === 'binaries') {
    const checks = await import('./lib/repository-checks.mjs');
    printReport(options.command === 'docs' ? checks.checkDocs(root) : checks.checkBinaries(root, { mode: options.history ? 'history' : options.ref ? 'tree' : 'staged', ref: options.ref || 'HEAD' }));
    return;
  }
  if (options.command === 'artifacts' || options.command === 'cache') {
    const artifacts = await import('./lib/artifacts.mjs');
    if (options.command === 'artifacts') printReport(options.action === 'list' ? artifacts.listArtifacts(root) : artifacts.pruneArtifacts(root, { apply: options.apply }));
    else printReport(options.action === 'status' ? artifacts.cacheStatus(root) : artifacts.cleanCache(root, { profile: options.profile, apply: options.apply }));
    return;
  }
  printReport(inspectTools());
  const locked = ['--locked', ...(options.offline ? ['--offline'] : [])];
  await managed(options, async (execute, run) => {
    if (options.command === 'bootstrap') {
      await execute('npm', ['ci', '--prefix', 'web', ...(options.offline ? ['--offline'] : [])], { PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: '1' });
      return;
    }
    if (options.command === 'e2e') {
      await execute('npm', ['run', 'test:e2e', '--prefix', 'web', '--', '--output', path.join(run.path, 'browser-results')], { PLAYWRIGHT_USE_SYSTEM_CHROME: '1', PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: '1' });
      return;
    }
    const testTools = () => execute(process.execPath, ['--test', ...fs.readdirSync(path.join(root, 'scripts/tests')).filter(name => name.endsWith('.test.mjs')).sort().map(name => `scripts/tests/${name}`)]);
    if (options.command === 'test-tools') { await testTools(); return; }
    if (options.command === 'check') {
      const checks = await import('./lib/repository-checks.mjs');
      printReport(checks.checkDocs(root));
      printReport(checks.checkBinaries(root, { mode: options.ci ? 'tree' : 'staged', ref: 'HEAD' }));
      await testTools();
      await execute('npm', ['run', 'typecheck', '--prefix', 'web']);
      await execute('npm', ['test', '--prefix', 'web']);
    }
    const releaseTarget = options.command === 'package'
      ? (await import('./lib/release.mjs')).releaseTarget(root) : undefined;
    await execute('npm', ['run', 'build', '--prefix', 'web']);
    if (options.command === 'check') {
      await execute('cargo', ['fmt', '--all', '--check']);
      await execute('cargo', ['clippy', ...locked, '--all-targets', '--', '-D', 'warnings']);
      await execute('cargo', ['test', ...locked, '--all-targets', '--', '--test-threads=4']);
      await execute('cargo', ['build', ...locked, '--bins']);
      await execute('cargo', ['build', ...locked, '--release', '--bins']);
    } else await execute('cargo', ['build', ...locked, '--bins', ...(options.release || options.command === 'package' ? ['--release'] : []), ...(releaseTarget ? ['--target', releaseTarget] : [])]);
    if (options.command === 'package') {
      const { packageRelease } = await import('./lib/release.mjs');
      const release = packageRelease(root, run.path, options);
      const { verifyRelease } = await import('./lib/release-smoke.mjs');
      const verification = await verifyRelease(release.archive, run.path, release.version);
      printReport({ ...release, ...verification });
    }
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
