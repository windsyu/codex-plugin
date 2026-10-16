#!/usr/bin/env node
// Opt-in real launchd probe. All application/native files belong to one
// temporary HOME; never registers login startup in the real user's Library.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { createRun } from './lib/artifacts.mjs';

assert.equal(process.platform, 'darwin', 'requires a logged-in macOS desktop');
const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const run = createRun(root, { label: 'real-launchagent-lifecycle' });
const home = path.join(run.path, 'home');
const native = path.join(home, 'native');
const binary = path.join(root, 'target/debug/codex-view');
fs.mkdirSync(native, { recursive: true, mode: 0o700 });
const cli = path.join(home, 'synthetic-codex');
fs.writeFileSync(cli, '#!/bin/sh\nif [ "$1" = --version ]; then echo "codex-cli 0.155.1"; exit 0; fi\necho "synthetic terminal ready"\nexec /bin/cat\n', { mode: 0o700 });
fs.writeFileSync(path.join(native, 'config.toml'), 'model="synthetic-model"\nmodel_provider="custom"\n[model_providers.custom]\nname="synthetic"\nbase_url="http://127.0.0.1:1/v1"\nwire_api="responses"\nrequires_openai_auth=false\nexperimental_bearer_token="synthetic"\n');
const env = { ...process.env, HOME: home, USERPROFILE: home, CODEX_HOME: native };
const options = { cwd: home, env, encoding: 'utf8', timeout: 65000 };
const checks = [];
let foreground;
function command(args, expected = 0) {
  const result = spawnSync(binary, args, options);
  assert.equal(result.status, expected, `command ${args[0]} failed: ${result.stderr || result.error || ''}`);
  return result;
}
function status() { return JSON.parse(command(['status', '--json']).stdout); }
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(predicate, description, timeout = 45000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await delay(150);
  }
  throw new Error(`timeout: ${description}`);
}
function alive(pid) { try { process.kill(pid, 0); return true; } catch { return false; } }
function entry() {
  const runtime = path.join(home, '.codex-web/runtime');
  const scope = fs.readdirSync(runtime).find(name => fs.existsSync(path.join(runtime, name, 'instance.json')));
  return JSON.parse(fs.readFileSync(path.join(runtime, scope, 'instance.json')));
}
async function privateShutdown() {
  const e = entry();
  const response = await fetch(`${e.address}/workbench/v1/application/control`, {
    method: 'POST', headers: { Origin: e.address, 'Content-Type': 'application/json' },
    body: JSON.stringify({ instanceId: e.instanceId, action: 'shutdown', token: new URL(e.url).hash.slice('#pair='.length) }),
  });
  assert.equal(response.status, 200);
}
let success = false;
try {
  console.log(`Task artifacts: ${run.path}`);
  const first = command(['--no-open', '--codex-bin', cli, 'start']);
  assert.match(first.stderr, /code-view open --relay/);
  let s = status();
  assert.equal(s.state, 'running'); assert.equal(s.autostart, false);
  assert.equal(s.application.activeRuns, 0); assert.ok(s.application.serviceLabel);
  assert.equal(s.application.relay.registered, false);
  const firstPid = s.application.processId;
  command(['start']);
  assert.equal(status().application.processId, firstPid);
  checks.push('background start, noninteractive registration guidance, terminal-independent lifetime, repeat start preserves defaults');
  command(['autostart', 'on']);
  assert.equal(status().autostart, true);
  assert.equal(status().application.processId, firstPid);
  command(['stop']);
  assert.equal(status().state, 'stopped'); assert.equal(status().autostart, true);
  command(['--no-open', 'start']);
  command(['autostart', 'off']);
  assert.equal(status().autostart, false);
  checks.push('autostart preferences do not alter the current process and survive stop');
  // A successful exit leaves a loaded job with KeepAlive=false: start must
  // kickstart it without killing any running process.
  const cleanPid = status().application.processId;
  await privateShutdown();
  await until(() => !alive(cleanPid), 'clean service exit');
  command(['--no-open', 'start']);
  assert.notEqual(status().application.processId, cleanPid);
  checks.push('loaded cleanly exited job can start again');
  let projects = [];
  for (const name of ['项目 with spaces', 'second-project']) {
    const project = path.join(home, name); fs.mkdirSync(project);
    const output = command(['--no-open', '--project', project]);
    projects.push(JSON.parse(output.stdout.trim()).cliPid);
  }
  assert.equal(status().application.activeRuns, 2);
  const crashed = status().application.processId;
  process.kill(crashed, 'SIGKILL');
  await until(() => {
    try { const next = status(); return next.application?.processId !== crashed && next.application?.activeRuns === 0; }
    catch { return false; }
  }, 'launchd limited restart without CLI replay', 60000);
  await until(() => projects.every(pid => !alive(pid)), 'crashed service children exit');
  checks.push('abnormal restart does not restore CLI projects or replay input');
  projects = [];
  for (const name of ['项目 with spaces', 'second-project']) {
    projects.push(JSON.parse(command(['--no-open', '--project', path.join(home, name)]).stdout.trim()).cliPid);
  }
  const config = path.join(home, '.codex-web/config/config.json');
  const saved = fs.readFileSync(config);
  fs.writeFileSync(config, '{ invalid');
  command(['stop']);
  assert.equal(status().state, 'stopped');
  assert.ok(projects.every(pid => !alive(pid)));
  fs.writeFileSync(config, saved);
  checks.push('malformed current settings do not prevent complete stop of all owned CLI projects');
  foreground = spawn(binary, ['--no-open'], { cwd: home, env, stdio: 'ignore' });
  await until(() => { try { return status().state === 'running'; } catch { return false; } }, 'foreground readiness');
  const conflict = command(['start'], 1);
  assert.match(conflict.stderr, /foreground instance/);
  command(['stop']);
  await until(() => foreground.exitCode !== null, 'foreground exits after authenticated stop');
  checks.push('foreground conflict is explicit; stop supports foreground ownership');
  success = true;
} finally {
  const cleanup = spawnSync(binary, ['stop'], options);
  if (cleanup.status !== 0) { success = false; console.error('cleanup failed; retain test HOME and inspect its private service.log'); }
  spawnSync(binary, ['autostart', 'off'], options);
  const maintenance = spawnSync(binary, ['service-maintenance', '--remove'], options);
  if (maintenance.status !== 0) { success = false; console.error('test registration removal failed'); }
  if (foreground && foreground.exitCode === null) {
    foreground.kill('SIGTERM');
    await until(() => foreground.exitCode !== null, 'test foreground cleanup').catch(() => { success = false; });
  }
  fs.writeFileSync(path.join(run.path, 'report.json'), JSON.stringify({ success, checks, limitations: ['Actual login/logout and sleep/resume are not simulated.'] }, null, 2));
  run.finish(success);
  console.log(JSON.stringify({ success, checks }));
  if (!success) process.exitCode = 1;
}
