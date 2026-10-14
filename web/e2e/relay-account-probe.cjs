// Isolated account-management regression probe. Uses only system Chrome and
// synthetic relay state; never emits passwords, cookies, database data or URLs.
// Run from the repository root after rebuilding: node web/e2e/relay-account-probe.cjs
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const http = require('node:http');
const crypto = require('node:crypto');
const { spawn } = require('node:child_process');
const { pathToFileURL } = require('node:url');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const { expect } = require('playwright/test');

const repo = path.resolve(__dirname, '../..');
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
let stage = 'setup', run, runtime, relay, chrome, finishing;
let relayExited = false, relaySpawnFailed = false;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

async function stopRelay() {
  if (!relay || relayExited || relaySpawnFailed) return;
  const exited = new Promise(resolve => relay.once('exit', resolve));
  relay.kill('SIGTERM');
  await Promise.race([exited, delay(3000)]);
  if (!relayExited) { relay.kill('SIGKILL'); await Promise.race([exited, delay(3000)]); }
  assert.ok(relayExited, 'owned relay did not stop');
}

function finish(success, details = {}) {
  if (finishing) return finishing;
  finishing = (async () => {
    let cleaned = true;
    try {
      if (chrome) {
        const ownedPid = chrome.process().pid;
        await Promise.race([chrome.close().catch(() => {}), delay(3000)]);
        await chrome.kill();
        let alive = true;
        try { process.kill(ownedPid, 0); } catch (error) { if (error.code === 'ESRCH') alive = false; else throw error; }
        assert.ok(!alive, 'owned Chrome did not stop');
      }
      await stopRelay();
      // Only this probe's synthetic runtime is removed, never shared artifacts.
      if (runtime) fs.rmSync(runtime, { recursive: true, force: true });
    } catch { cleaned = false; success = false; }
    const result = { stage: success ? 'passed' : 'failed', check: stage, ...details, cleanup: cleaned };
    if (run) {
      fs.writeFileSync(path.join(run.path, 'result.json'), JSON.stringify(result, null, 2) + '\n', { mode: 0o600 });
      run.finish(success);
    }
    process.stdout.write(JSON.stringify(result) + '\n');
    process.exit(success ? 0 : 1);
  })();
  return finishing;
}

for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.on(signal, () => void finish(false, { reason: 'signal' }));
setTimeout(() => void finish(false, { reason: 'deadline' }), 90000).unref();

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

function ready(port) {
  return new Promise(resolve => {
    // node:http preserves the explicit virtual Host unlike fetch implementations.
    const request = http.get({ hostname: '127.0.0.1', port, path: '/login', headers: { Host: `app.localhost:${port}` } }, response => {
      response.resume(); resolve(response.statusCode === 200);
    });
    request.setTimeout(1000, () => request.destroy());
    request.on('error', () => resolve(false));
  });
}

async function listUsers(page) {
  return page.evaluate(async () => {
    const response = await fetch('/relay/v1/admin/users', { cache: 'no-store' });
    if (!response.ok) throw new Error('list_failed');
    return (await response.json()).users;
  });
}

async function login(page, username, password) {
  await expect(page.locator('#signed-out')).toBeVisible();
  await page.locator('#login [name=username]').fill(username);
  await page.locator('#login [name=password]').fill(password);
  await page.locator('#login button').click();
  await expect(page.locator('#signed-in')).toBeVisible();
}

async function changePassword(page, oldPassword, password) {
  await expect(page.locator('#forced-change')).toBeVisible();
  assert.equal(new URL(page.url()).pathname, '/account/security');
  await expect(page.locator('#navigation')).toBeHidden();
  await page.locator('#change [name=oldPassword]').fill(oldPassword);
  await page.locator('#change [name=password]').fill(password);
  await page.locator('#change button').click();
  await expect(page.locator('#signed-out')).toBeVisible();
  await expect(page.locator('#status')).toContainText('密码已修改');
}

(async () => {
  const { createRun } = await import(pathToFileURL(path.join(repo, 'scripts/lib/artifacts.mjs')).href);
  run = createRun(repo, { label: 'relay-account-system-chrome' });
  runtime = path.join(run.path, 'synthetic-runtime');
  fs.mkdirSync(runtime, { mode: 0o700 });
  const home = path.join(runtime, 'home'), codexHome = path.join(runtime, 'codex-home');
  fs.mkdirSync(home, { mode: 0o700 }); fs.mkdirSync(codexHome, { mode: 0o700 });
  const bootstrap = 'Synthetic bootstrap ' + crypto.randomUUID();
  const adminPassword = 'Synthetic replacement ' + crypto.randomUUID();
  const temporary = 'Synthetic user temporary ' + crypto.randomUUID();
  const userPassword = 'Synthetic user replacement ' + crypto.randomUUID();
  const passwordFile = path.join(runtime, 'bootstrap-password');
  fs.writeFileSync(passwordFile, bootstrap, { mode: 0o600, flag: 'wx' });
  const port = await freePort(), origin = `http://app.localhost:${port}`;
  const env = { PATH: process.env.PATH, HOME: home, USERPROFILE: home, CODEX_HOME: codexHome, TMPDIR: runtime };
  stage = 'relay-start';
  relay = spawn(path.join(repo, 'target/debug/codex-relayd'), [
    '--data-dir', path.join(runtime, 'relay-data'), '--portal-origin', origin,
    '--single-origin', '--allow-insecure-loopback', 'serve', '--bind', `127.0.0.1:${port}`,
    '--bootstrap-password-file', passwordFile,
  ], { cwd: runtime, env, stdio: 'ignore' });
  relay.once('exit', () => { relayExited = true; });
  relay.once('error', () => { relaySpawnFailed = true; });
  const readyDeadline = Date.now() + 15000;
  while (true) {
    assert.ok(!relayExited && !relaySpawnFailed, 'relay_start_failed');
    if (await ready(port)) break;
    assert.ok(Date.now() < readyDeadline, 'relay_start_deadline');
    await delay(100);
  }
  stage = 'chrome-start';
  chrome = await chromium.launchServer({
    executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: true, host: '127.0.0.1', port: 0, timeout: 10000, env,
    handleSIGINT: false, handleSIGTERM: false, handleSIGHUP: false,
    args: ['--no-proxy-server', '--host-resolver-rules=MAP app.localhost 127.0.0.1'],
  });
  const browser = await chromium.connect(chrome.wsEndpoint());
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  const page = await context.newPage(); page.setDefaultTimeout(12000);
  let pageErrors = 0, deletes = 0;
  page.on('pageerror', () => pageErrors++);
  page.on('request', request => { if (request.method() === 'DELETE') deletes++; });
  await page.goto(origin + '/login');
  stage = 'admin-first-login';
  await login(page, 'windsyu', bootstrap);
  await expect(page.locator('#account')).toHaveText('windsyu');
  await changePassword(page, bootstrap, adminPassword);
  stage = 'admin-relogin';
  await login(page, 'windsyu', adminPassword);
  await expect(page.locator('#account')).toHaveText('windsyu');
  await expect(page.locator('#admin-link')).toBeVisible();
  await page.locator('#admin-link').click();
  await expect(page.locator('#administration')).toBeVisible();
  assert.equal(new URL(page.url()).pathname, '/admin/users');

  // Deliberately use the former admin login name as an ordinary account.
  const username = 'admin';
  async function createUser() {
    await page.locator('#create-user [name=username]').fill(username);
    await page.locator('#create-user [name=temporaryPassword]').fill(temporary);
    await page.locator('#create-user button').click();
    await expect(page.locator('#users h2')).toHaveText(username);
    const users = await listUsers(page);
    assert.equal(users.length, 1); assert.ok(uuid.test(users[0].accountId));
    return users[0].accountId;
  }
  stage = 'create-ordinary-admin-name';
  const firstId = await createUser();
  const ordinary = await browser.newContext();
  const userPage = await ordinary.newPage(); userPage.setDefaultTimeout(12000);
  userPage.on('pageerror', () => pageErrors++);
  await userPage.goto(origin + '/login');
  await login(userPage, username, temporary);
  await changePassword(userPage, temporary, userPassword);
  await login(userPage, username, userPassword);
  await expect(userPage.locator('#admin-link')).toBeHidden();
  const ordinaryAccount = await userPage.evaluate(async () => (await fetch('/relay/v1/account')).json());
  assert.equal(ordinaryAccount.role, 'user');
  assert.equal(ordinaryAccount.accountId, firstId);

  stage = 'cancel-delete';
  let cancelWarning = false;
  const canceledDialog = page.waitForEvent('dialog').then(async dialog => {
    cancelWarning = dialog.type() === 'confirm' && dialog.message().includes('不可撤销') && dialog.message().includes('不删除电脑上的 Codex 历史');
    await dialog.dismiss();
  });
  await page.locator('#users').getByRole('button', { name: '删除账号', exact: true }).click();
  await canceledDialog;
  assert.ok(cancelWarning);
  await expect(page.locator('#users').getByRole('button', { name: '删除账号', exact: true })).toBeEnabled();
  assert.equal(deletes, 0); assert.equal((await listUsers(page))[0].accountId, firstId);

  stage = 'failed-delete-keeps-row';
  await page.route(`**/relay/v1/admin/users/${firstId}`, route => route.fulfill({ status: 503, body: '{}' }));
  page.once('dialog', dialog => void dialog.accept());
  await page.locator('#users').getByRole('button', { name: '删除账号', exact: true }).click();
  await expect(page.locator('#status')).toContainText('服务暂时不可用');
  await expect(page.locator('#users h2')).toHaveText(username);
  assert.equal((await listUsers(page))[0].accountId, firstId);
  await page.unroute(`**/relay/v1/admin/users/${firstId}`);

  stage = 'confirm-delete';
  const beforeDelete = deletes;
  page.once('dialog', dialog => void dialog.accept());
  await page.locator('#users').getByRole('button', { name: '删除账号', exact: true }).click();
  await expect(page.locator('#status')).toContainText('账号已删除');
  await expect(page.locator('#users h2')).toHaveCount(0);
  assert.equal(deletes, beforeDelete + 1); assert.equal((await listUsers(page)).length, 0);
  const revoked = await userPage.evaluate(async () => (await fetch('/relay/v1/account')).status);
  assert.equal(revoked, 401);

  stage = 'same-name-recreation';
  const replacementId = await createUser(); assert.notEqual(replacementId, firstId);
  stage = 'hostile-text-escaping';
  const hostile = '<img src=x onerror="window.__relayProbeXss=true">';
  await page.route('**/relay/v1/admin/users', route => {
    if (route.request().method() !== 'GET') return route.continue();
    return route.fulfill({ json: { users: [{ accountId: replacementId, username: hostile, disabled: false, mustChangePassword: true }] } });
  });
  await page.reload();
  await expect(page.locator('#users h2')).toHaveText(hostile);
  await expect(page.locator('#users img')).toHaveCount(0);
  assert.equal(await page.evaluate(() => Boolean(window.__relayProbeXss)), false);
  await page.unroute('**/relay/v1/admin/users');
  await page.reload(); await expect(page.locator('#users h2')).toHaveText(username);
  const dimensions = await page.evaluate(() => ({ viewport: document.documentElement.clientWidth, content: document.documentElement.scrollWidth }));
  assert.ok(dimensions.content <= dimensions.viewport + 1);
  assert.equal(pageErrors, 0);
  stage = 'complete';
  await finish(true, { chrome: browser.version(), mobileWidth: 390, adminDisplay: true, mandatoryPasswordChange: true,
    ordinaryAdminName: true, cancelNoMutation: true, failureKeepsRow: true, confirmedDeletion: true,
    revokedUserSession: true, recreationNewId: true, hostileTextEscaped: true, pageErrors });
})().catch(error => void finish(false, { kind: ['AssertionError', 'TimeoutError'].includes(error?.name) ? error.name : 'operation_failed' }));
