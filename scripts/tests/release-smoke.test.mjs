import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { waitForSyntheticLaunches } from '../lib/release-smoke.mjs';

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'release-launch-ledger-'));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return path.join(directory, 'launched-cwds');
}

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
