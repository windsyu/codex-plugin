import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

function run(program, args, options) {
  const result = spawnSync(program, args, { ...options, encoding: 'utf8', timeout: 15000 });
  assert.equal(result.status, 0, `${program}: ${result.error?.message || result.stderr}`);
  return result.stdout.trim();
}
async function until(check, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    const result = check();
    if (result) return result;
    await delay(25);
  }
  throw new Error(`Release verification timed out: ${description}`);
}
export async function verifyRelease(archive, workspace, version) {
  const root = path.join(workspace, '安装验证 with spaces');
  fs.mkdirSync(root);
  run('tar', ['-xzf', archive, '-C', root]);
  const release = path.join(root, path.basename(archive, '.tar.gz'));
  const home = path.join(root, 'isolated-home');
  const native = path.join(home, '.codex');
  const project = path.join(root, '项目 with spaces');
  const other = path.join(root, '另一个项目');
  const prefix = path.join(home, '.codex-view');
  for (const directory of [native, project, other]) fs.mkdirSync(directory, { recursive: true });
  const env = { ...process.env, HOME: home, USERPROFILE: home, CODEX_HOME: native, PATH: `${prefix}/bin:${process.env.PATH}` };
  const options = { cwd: project, env };
  run('/bin/sh', [path.join(release, 'install.sh'), '--prefix', prefix], options);
  assert.equal(run('code-view', ['--version'], options), `codex-view ${version}`);
  const fakeCli = path.join(root, 'synthetic-codex');
  fs.writeFileSync(fakeCli, '#!/bin/sh\nif [ "$1" = --version ]; then echo "codex-cli 0.159.2"; exit 0; fi\nprintf "%s\\n" "$PWD" >> "$CODEX_HOME/launched-cwds"\nexec /bin/cat\n', { mode: 0o755 });
  fs.writeFileSync(path.join(native, 'config.toml'), 'model="synthetic-release"\nmodel_provider="custom"\n[model_providers.custom]\nname="synthetic"\nbase_url="http://127.0.0.1:1/v1"\nwire_api="responses"\nrequires_openai_auth=false\nexperimental_bearer_token="synthetic-release-token"\n');
  const entryFile = path.join(root, 'private-entry.json');
  const child = spawn('code-view', ['--no-open', '--codex-bin', fakeCli, '--entry-file', entryFile], { ...options, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', spawnError;
  child.stdout.on('data', data => { stdout += data; });
  child.stderr.on('data', data => { stderr += data; });
  child.on('error', error => { spawnError = error; });
  const cliPids = [];
  try {
    const ready = await until(() => {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null) throw new Error(`Installed launcher exited early: ${stderr}`);
      return stdout.split('\n').slice(0, -1).filter(line => line.startsWith('{')).map(line => JSON.parse(line)).find(line => line.stage === 'run-ready');
    }, 'current-directory project launch');
    cliPids.push(ready.cliPid);
    const entry = JSON.parse(fs.readFileSync(entryFile, 'utf8'));
    assert.equal(entry.instanceId, ready.instanceId);
    assert.equal(fs.readFileSync(path.join(native, 'launched-cwds'), 'utf8').trim(), project);
    const page = await fetch(entry.address);
    assert.equal(page.status, 200);
    const html = await page.text();
    const script = html.match(/src="([^"\n]+\.js)"/);
    assert.ok(script, 'embedded homepage references a script');
    const asset = await fetch(new URL(script[1], entry.address));
    assert.equal(asset.status, 200);
    assert.ok((await asset.text()).length > 1000, 'embedded script has content');
    const pairToken = new URL(entry.url).hash.slice('#pair='.length);
    const paired = await fetch(`${entry.address}/workbench/v1/pair`, { method: 'POST', headers: { Origin: entry.address, 'Content-Type': 'application/json' }, body: JSON.stringify({ token: pairToken }) });
    assert.equal(paired.status, 204);
    const cookie = paired.headers.get('set-cookie').split(';')[0];
    const getApplication = async () => {
      const response = await fetch(`${entry.address}/workbench/v1/application`, { headers: { Cookie: cookie } });
      assert.equal(response.status, 200);
      return response.json();
    };
    assert.equal((await getApplication()).runs.length, 1);
    const reused = JSON.parse(run('code-view', ['--no-open', '--codex-bin', fakeCli], options));
    assert.equal(reused.stage, 'reused');
    assert.equal(reused.cliPid, ready.cliPid);
    run('code-view', ['--history', '--no-open'], options);
    assert.equal((await getApplication()).runs.length, 1, 'history does not start CLI');
    const second = JSON.parse(run('code-view', [other, '--no-open', '--codex-bin', fakeCli], options));
    cliPids.push(second.cliPid);
    assert.equal((await getApplication()).runs.length, 2);
    assert.deepEqual(fs.readFileSync(path.join(native, 'launched-cwds'), 'utf8').trim().split('\n').sort(), [project, other].sort());
  } finally {
    if (child.exitCode === null) child.kill('SIGINT');
    try { await until(() => child.exitCode !== null || child.signalCode !== null, 'launcher shutdown'); }
    catch (error) { child.kill('SIGKILL'); throw error; }
    assert.equal(child.exitCode, 0, stderr);
    for (const pid of cliPids) {
      await until(() => {
        try { process.kill(pid, 0); return false; }
        catch (error) { if (error.code !== 'ESRCH') throw error; return true; }
      }, 'owned CLI shutdown');
    }
    assert.equal(fs.existsSync(entryFile), false, 'private entry removed on exit');
  }
  return { verification: 'passed', cases: ['archive extraction', 'isolated install', 'version', 'current project', 'embedded page/assets', 'same-project reuse', 'history without CLI', 'explicit second project', 'SIGINT cleanup'] };
}
