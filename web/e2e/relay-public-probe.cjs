// System Chrome probe for a live synthetic relay. Input is one JSON document on stdin.
// Neither credentials nor page bodies are written to stdout, stderr, or files.
const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const { expect } = require('playwright/test');

let stage = 'input';
let server;
let launching;
let closing;
let reported = false;
let diagnostic = {};
const report = value => { if (!reported) { process.stdout.write(`${JSON.stringify(value)}\n`); reported = true; } };
const uuidHost = /^g-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\.localhost$/i;

function fixture() {
  const raw = fs.readFileSync(0, 'utf8');
  assert.ok(raw.length > 0 && raw.length < 65536);
  const input = JSON.parse(raw);
  assert.ok(Array.isArray(input.gateways) && input.gateways.length === 2);
  const portal = new URL(input.portalOrigin);
  assert.equal(portal.protocol, 'http:');
  assert.ok(portal.hostname === 'app.localhost' || portal.hostname === '127.0.0.1');
  assert.equal(portal.pathname, '/');
  assert.equal(portal.search, ''); assert.equal(portal.hash, '');
  for (const gateway of input.gateways) {
    const url = new URL(gateway.origin);
    assert.equal(url.protocol, 'http:');
    assert.match(url.hostname, uuidHost);
    assert.equal(url.port, portal.port);
    assert.equal(url.pathname, '/');
    assert.equal(url.search, ''); assert.equal(url.hash, '');
    assert.match(gateway.cookie, /^[^=;]+=[^;]+$/);
    assert.match(gateway.accountCookie, /^[^=;]+=[^;]+$/);
    assert.equal(typeof gateway.expectedInstanceId, 'string');
    assert.ok(gateway.expectedInstanceId.length > 0);
    if (gateway.runId != null) assert.equal(typeof gateway.runId, 'string');
  }
  assert.notEqual(input.gateways[0].origin, input.gateways[1].origin);
  if (input.cwd != null) assert.equal(typeof input.cwd, 'string');
  return input;
}

function cookie(value, origin) {
  const split = value.indexOf('=');
  // Install the actual Secure host-only cookie against a virtual HTTPS origin.
  // The synthetic relay still serves HTTP on loopback; the subsequent request
  // assertion checks that Chrome really sends this cookie on that origin.
  const secureOrigin = new URL(origin);
  secureOrigin.protocol = 'https:';
  return { name: value.slice(0, split), value: value.slice(split + 1), url: secureOrigin.href, secure: true, sameSite: 'Strict' };
}

async function shutdown(code) {
  if (closing) return closing;
  closing = (async () => {
    await launching?.catch(() => {});
    if (server) {
      let timer;
      const done = await Promise.race([
        server.close().then(() => true, () => false),
        new Promise(resolve => { timer = setTimeout(() => resolve(false), 3000); }),
      ]);
      clearTimeout(timer);
      if (!done) await server.kill().catch(() => {});
    }
    process.exit(code);
  })().catch(() => process.exit(1));
  return closing;
}

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => { report({ stage: 'failed', check: stage }); void shutdown(1); });
process.stdout.on('error', () => void shutdown(1));
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); void shutdown(1); }, 90000).unref();

async function portal(browser, input, gateway, index) {
  stage = `portal-${index}`;
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  stage = `portal-${index}-cookie`;
  await context.addCookies([cookie(gateway.accountCookie, input.portalOrigin)]);
  diagnostic = { cookieStored: (await context.cookies(input.portalOrigin)).some(item => item.name === gateway.accountCookie.slice(0, gateway.accountCookie.indexOf('='))) };
  const page = await context.newPage();
  page.setDefaultTimeout(10000);
  stage = `portal-${index}-navigation`;
  const response = await page.goto(input.portalOrigin, { waitUntil: 'domcontentloaded' });
  diagnostic.portalHttp = response?.status() ?? null;
  diagnostic.titleExpected = (await page.title()) === 'Codex workbenches';
  const headers = await response.request().allHeaders();
  diagnostic.cookieSent = !!headers.cookie?.split(/;\s*/).includes(gateway.accountCookie);
  assert.equal(response?.status(), 200);
  stage = `portal-${index}-account`;
  diagnostic.accountHttp = await page.evaluate(async () => (await fetch('/relay/v1/account', { credentials: 'same-origin', cache: 'no-store' })).status);
  assert.equal(diagnostic.accountHttp, 200);
  stage = `portal-${index}-render`;
  await expect(page.locator('#signed-in')).toBeVisible();
  stage = `portal-${index}-gateways`;
  const current = await page.evaluate(async () => {
    const response = await fetch('/relay/v1/gateways', { credentials: 'same-origin', cache: 'no-store' });
    return { status: response.status, body: response.ok ? await response.json() : null };
  });
  assert.equal(current.status, 200);
  assert.ok(Array.isArray(current.body?.gateways));
  const gatewayId = new URL(gateway.origin).hostname.match(uuidHost)[1];
  assert.ok(current.body.gateways.some(item => item.gatewayId === gatewayId));
  await expect(page.locator('#gateways li')).toHaveCount(current.body.gateways.length);
  await context.close();
  return current.body.gateways.map(item => item.gatewayId);
}

async function workbench(browser, input, gateway, index) {
  stage = `workbench-${index}`;
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  await context.addCookies([cookie(gateway.cookie, gateway.origin)]);
  const page = await context.newPage();
  page.setDefaultTimeout(12000);
  let pageErrors = 0;
  let assetOk = 0;
  let pickerCalls = 0;
  page.on('pageerror', () => { pageErrors++; });
  page.on('response', response => {
    const url = new URL(response.url());
    if (url.origin === gateway.origin && url.pathname.startsWith('/assets/') && response.status() === 200) assetOk++;
  });
  page.on('request', request => { if (new URL(request.url()).pathname === '/workbench/v1/application/pick-directory') pickerCalls++; });
  const response = await page.goto(gateway.origin, { waitUntil: 'domcontentloaded' });
  assert.equal(response?.status(), 200);
  await expect(page.locator('.wb-home')).toBeVisible();
  assert.ok(assetOk > 0);
  const app = await page.evaluate(async () => {
    const response = await fetch('/workbench/v1/application', { credentials: 'same-origin', cache: 'no-store' });
    return { status: response.status, body: response.ok ? await response.json() : null };
  });
  assert.equal(app.status, 200);
  assert.equal(app.body.instanceId, gateway.expectedInstanceId);
  assert.equal(app.body.remoteOwner, true);
  assert.equal(app.body.directoryPickerAvailable, false);
  await expect(page.getByRole('button', { name: '公网接入', exact: true })).toHaveCount(0);
  await expect(page.locator('.wb-relay-panel')).toHaveCount(0);
  if (input.cwd && app.body.launchAvailable) {
    stage = `path-input-${index}`;
    await page.getByRole('button', { name: '打开其他目录', exact: true }).click();
    await expect(page.getByLabel('项目目录', { exact: true })).toBeVisible();
    await page.getByLabel('项目目录', { exact: true }).fill(input.cwd);
    await expect(page.locator('.wb-launch-pick')).toHaveCount(0);
    assert.equal(pickerCalls, 0);
    await page.locator('.wb-launch-close').click();
  }
  stage = `settings-${index}`;
  await page.getByRole('button', { name: '设置', exact: true }).click();
  const settings = page.locator('#wb-settings-panel');
  await expect(settings).toBeVisible();
  await expect(settings.locator('#wb-settings-form')).toBeVisible();
  if (gateway.runId) {
    stage = `run-${index}`;
    const runUrl = new URL(gateway.origin);
    runUrl.searchParams.set('run', gateway.runId);
    const runResponse = await page.goto(runUrl.href, { waitUntil: 'domcontentloaded' });
    assert.equal(runResponse?.status(), 200);
    await expect(page.locator('.wb-terminal')).toBeVisible();
    await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true', { timeout: 15000 });
    const read = await page.evaluate(async id => {
      const response = await fetch(`/workbench/v1/runs/${encodeURIComponent(id)}/run`, { credentials: 'same-origin', cache: 'no-store' });
      return { status: response.status, epoch: response.ok ? (await response.json()).runEpoch : null };
    }, gateway.runId);
    assert.equal(read.status, 200); assert.equal(read.epoch, gateway.runId);
  }
  assert.equal(pageErrors, 0);
  assert.equal(pickerCalls, 0);
  await context.close();
  return { assets: assetOk, run: !!gateway.runId };
}

async function crossCookie(browser, a, b) {
  stage = 'cross-account-cookie';
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  await context.addCookies([cookie(a.cookie, b.origin)]);
  const page = await context.newPage();
  const response = await page.goto(new URL('/workbench/v1/application', b.origin).href, { waitUntil: 'domcontentloaded' });
  const sent = await response.request().allHeaders();
  assert.ok(sent.cookie?.split(/;\s*/).includes(a.cookie));
  assert.ok([401, 403, 404].includes(response?.status()));
  await context.close();
}

(async () => {
  const input = fixture();
  stage = 'chrome';
  launching = chromium.launchServer({
    executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: true,
    host: '127.0.0.1', port: 0, timeout: 10000,
    handleSIGINT: false, handleSIGTERM: false, handleSIGHUP: false,
    args: ['--no-proxy-server', '--host-resolver-rules=MAP * 127.0.0.1, EXCLUDE localhost'],
  });
  server = await launching;
  const browser = await chromium.connect(server.wsEndpoint(), { timeout: 10000 });
  const ownership = [];
  for (let i = 0; i < input.gateways.length; i++) ownership.push(await portal(browser, input, input.gateways[i], i));
  stage = 'portal-isolation';
  for (let i = 0; i < 2; i++) {
    const otherId = new URL(input.gateways[1 - i].origin).hostname.match(uuidHost)[1];
    assert.ok(!ownership[i].includes(otherId));
  }
  const results = [];
  for (let i = 0; i < input.gateways.length; i++) results.push(await workbench(browser, input, input.gateways[i], i));
  await crossCookie(browser, input.gateways[0], input.gateways[1]);
  report({ stage: 'passed', chrome: browser.version(), mobileWidth: 390, portals: 2, workbenches: 2, assetResponses: results.map(result => result.assets), runs: results.filter(result => result.run).length, crossAccountDenied: true });
  await shutdown(0);
})().catch(error => {
  const message = typeof error?.message === 'string' ? error.message : '';
  const cookieReason = /Storage\.setCookies/.test(message) ? 'storage_set_cookies' : /Invalid cookie fields/.test(message) ? 'invalid_cookie_fields' : /Cookie.*(Secure|secure)/.test(message) ? 'secure_cookie_rejected' : undefined;
  report({ stage: 'failed', check: stage, kind: ['AssertionError', 'TimeoutError'].includes(error?.name) ? error.name : 'operation_failed', ...(stage.includes('-cookie') ? { cookieReason: cookieReason || 'other_cookie_error' } : {}), ...diagnostic });
  void shutdown(1);
});
