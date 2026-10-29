import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { waitForSyntheticLaunches, writeSyntheticCli } from '../lib/release-smoke.mjs';

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'release-launch-ledger-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return path.join(directory, 'launched-cwds');
}

test('release synthetic CLI answers metadata reads without recording a TUI launch', t => {
  const ledger = fixture(t);
  const home = fs.realpathSync(path.dirname(ledger));
  const cli = path.join(home, 'synthetic-codex');
  writeSyntheticCli(cli);
  const options = { cwd: home, env: { ...process.env, HOME: home, USERPROFILE: home, CODEX_HOME: home }, encoding: 'utf8', timeout: 5000 };
  const requests = [
    { id: 1, method: 'initialize' },
    { method: 'initialized' },
    { id: 2, method: 'config/read' },
    { id: 3, method: 'configRequirements/read' },
    { id: 4, method: 'account/read' },
    { id: 5, method: 'config/read' },
  ];
  const result = spawnSync(cli, ['-c', 'features.plugins=false', 'app-server', '--stdio'], {
    ...options, input: requests.map(request => JSON.stringify(request)).join('\n') + '\n',
  });
  assert.equal(result.status, 0, result.error?.message || result.stderr);
  const responses = result.stdout.trim().split('\n').map(line => JSON.parse(line));
  assert.deepEqual(responses.map(response => response.id), [1, 2, 3, 4, 5]);
  assert.deepEqual(responses[0].result, {});
  const config = {
    config: {
      model: 'synthetic-release', model_provider: 'custom',
      model_providers: { custom: {
        name: 'synthetic', base_url: 'http://127.0.0.1:1/v1', wire_api: 'responses',
        requires_openai_auth: false, experimental_bearer_token: 'synthetic-release-token',
      } },
    },
    layers: [],
  };
  assert.deepEqual(responses[1].result, config);
  assert.deepEqual(responses[4].result, config);
  assert.deepEqual(responses[2].result, { requirements: null });
  assert.deepEqual(responses[3].result, { account: null });
  assert.equal(fs.existsSync(ledger), false, 'metadata helper is not a TUI launch');
  const version = spawnSync(cli, ['--version'], options);
  assert.equal(version.status, 0);
  assert.equal(version.stdout, 'codex-cli 0.159.2\n');
  assert.equal(fs.existsSync(ledger), false, 'version lookup is not a TUI launch');
  const tui = spawnSync(cli, [], { ...options, input: 'synthetic terminal input\n' });
  assert.equal(tui.status, 0, tui.error?.message || tui.stderr);
  assert.equal(tui.stdout, 'synthetic terminal input\n');
  assert.equal(fs.readFileSync(ledger, 'utf8'), `${home}\n`, 'ordinary TUI records cwd exactly once');
});

// Schedule the script's observable work after the verifier starts, just as a
// spawned CLI may execute after the backend has already reported run-ready.
async function observeDelayedWrite(file, expected, write) {
  const observed = waitForSyntheticLaunches(file, expected);
  const written = new Promise((resolve, reject) => setImmediate(() => {
    try { write(); resolve(); } catch (error) { reject(error); }
  }));
  const results = await Promise.allSettled([observed, written]);
  for (const result of results) assert.equal(result.status, 'fulfilled', result.reason?.message);
}

test('release smoke waits for script execution after process readiness', async t => {
  const file = fixture(t);
  await observeDelayedWrite(file, ['/项目 with spaces'], () => fs.writeFileSync(file, '/项目 with spaces\n'));
});

test('release smoke waits for the explicit second project without accepting a smaller launch count', async t => {
  const file = fixture(t);
  fs.writeFileSync(file, '/first\n');
  await observeDelayedWrite(file, ['/first', '/second'], () => fs.appendFileSync(file, '/second\n'));
});

test('release smoke waits for complete records and preserves cwd whitespace', async t => {
  const file = fixture(t);
  fs.writeFileSync(file, '/project with ');
  await observeDelayedWrite(file, ['/project with spaces '], () => fs.appendFileSync(file, 'spaces \n'));
});

test('release smoke does not retry filesystem errors other than a not-yet-created ledger', async t => {
  const file = fixture(t);
  fs.mkdirSync(file);
  await assert.rejects(waitForSyntheticLaunches(file, ['/project']), { code: 'EISDIR' });
});

test('release smoke retains the bounded timeout when the script never records a launch', async t => {
  const file = fixture(t);
  await assert.rejects(waitForSyntheticLaunches(file, ['/project']), /Release verification timed out: synthetic CLI launch cwd records/);
});

test('release smoke rejects duplicate launches and wrong cwd, including fewer wrong entries', async t => {
  const file = fixture(t);
  for (const [actual, expected] of [
    ['/first\n/first\n', ['/first']],
    ['/first\n/first\n', ['/first', '/second']],
    ['/wrong\n', ['/first']],
    ['/wrong\n', ['/first', '/second']],
  ]) {
    fs.writeFileSync(file, actual);
    await assert.rejects(waitForSyntheticLaunches(file, expected), { code: 'ERR_ASSERTION' });
  }
});
