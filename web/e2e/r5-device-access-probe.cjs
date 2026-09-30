const { readRun, runApiPath } = require('./workbench-api.cjs');
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const jsQR = require('jsqr');
const assert = require('node:assert/strict');
let stage = 'launch', owner, phone;
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const run = page => readRun(page, '/run');
const status = page => readRun(page, '/access');
async function qr() {
  let result;
  await expect.poll(async () => {
    const pixels = await owner.locator('.wb-access-qr canvas').evaluate(canvas => ({
      data: Array.from(canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data), width: canvas.width, height: canvas.height,
    }));
    result = jsQR(new Uint8ClampedArray(pixels.data), pixels.width, pixels.height);
    return !!result;
  }).toBe(true);
  return result.data;
}
async function shot(page, suffix) {
  const prefix = process.env.WORKBENCH_PROBE_SCREENSHOT;
  if (prefix) await page.screenshot({ path: `${prefix}.${suffix}.png`, fullPage: true,
    // Screenshots never contain pairing codes, including their QR encoding.
    mask: [page.locator('.wb-access-qr canvas'), page.locator('.wb-access-copy')], maskColor: '#52657a' });
}
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 50000).unref();
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true,
    args: ['--no-proxy-server', '--host-resolver-rules=MAP machine.tail-test.ts.net 127.0.0.1'] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 980 } });
  owner = await context.newPage(); owner.setDefaultTimeout(6000);
  let errors = 0, sockets = 0;
  owner.on('pageerror', () => errors++);
  owner.on('websocket', s => { if (s.url().includes('/terminal')) sockets++; });
  stage = 'local'; await owner.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(owner.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  const terminal = await owner.locator('.xterm-helper-textarea').elementHandle();
  const before = await run(owner);
  stage = 'panel-keyboard';
  const access = owner.getByRole('button', { name: '手机接入', exact: true });
  await access.click();
  await expect(owner.getByRole('region', { name: '手机接入', exact: true })).toBeVisible();
  await owner.keyboard.press('Escape');
  await expect(owner.locator('#wb-access-panel')).toBeHidden();
  await expect(access).toBeFocused();
  stage = 'enable'; await access.click();
  const enableStarted = Date.now();
  await owner.getByRole('button', { name: '开启设备访问', exact: true }).click();
  await expect(owner.locator('.wb-access-qr canvas')).toBeVisible();
  stage = 'decode-ip'; const ip = await qr();
  const enableToQrMs = Date.now() - enableStarted;
  await owner.locator('.wb-access-address').filter({ hasText: 'Tailscale' }).getByRole('button', { name: '二维码', exact: true }).click();
  stage = 'decode-dns'; await expect.poll(async () => new URL(await qr()).hostname).toBe('machine.tail-test.ts.net');
  const dns = await qr(); assert.equal(new URL(ip).hash, new URL(dns).hash);
  assert.notEqual(new URL(ip).hostname, new URL(dns).hostname);
  const revision = (await status(owner)).revision;
  await owner.locator('.wb-access-address').filter({ hasText: '局域网' }).getByRole('button', { name: '二维码', exact: true }).click();
  stage = 'reselect-ip'; await expect.poll(qr).toBe(ip); assert.equal(await qr(), ip); assert.equal((await status(owner)).revision, revision);
  await shot(owner, 'desktop');
  stage = 'pair-http-phone';
  const mobile = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true,
    userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile Safari/604.1' });
  phone = await mobile.newPage(); phone.setDefaultTimeout(6000); phone.on('pageerror', () => errors++);
  await phone.goto(dns); await expect(phone.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  assert.equal(await phone.evaluate(() => isSecureContext), false);
  assert.equal(await phone.evaluate(() => typeof crypto.randomUUID), 'undefined');
  assert.equal(new URL(phone.url()).hash, ''); assert.equal(await phone.evaluate(() => document.cookie), '');
  assert.equal((await run(phone)).processId, before.processId); assert.equal((await run(phone)).runEpoch, before.runEpoch);
  await expect(phone.getByRole('button', { name: '手机接入', exact: true })).toHaveCount(0);
  stage = 'second-address';
  await expect(owner.locator('.wb-access-device')).toHaveCount(1);
  await expect(owner.locator('.wb-access-qr canvas')).toBeVisible();
  await expect(owner.locator('#wb-access-panel')).toContainText('本次启动期间有效，可重复扫码');
  await expect(owner.getByRole('button', { name: /生成配对链接/ })).toHaveCount(0);
  const cookies = await mobile.cookies(); const beforeRepeat = (await status(owner)).revision;
  await phone.goto(dns); await expect(phone.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  assert.deepEqual(await mobile.cookies(), cookies, 'repeat scan keeps the browser authorization');
  assert.equal((await status(owner)).revision, beforeRepeat);
  const secondLink = await qr(); assert.equal(secondLink, ip);
  const otherContext = await browser.newContext(); const other = await otherContext.newPage(); other.setDefaultTimeout(6000);
  await other.goto(secondLink); await expect(other.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  assert.equal((await run(other)).processId, before.processId);
  await expect(owner.locator('.wb-access-device')).toHaveCount(2);
  stage = 'stream'; report({ stage: 'progress', check: 'phone-stream' });
  await expect(phone.locator('.wb-message-list')).toContainText('手机流式中间内容');
  await expect(other.locator('.wb-message-list')).toContainText('手机流式中间内容');
  stage = 'input-and-reconnect';
  await phone.getByRole('button', { name: '在此输入', exact: true }).click();
  await expect(phone.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await phone.locator('.xterm-helper-textarea').focus(); await phone.keyboard.insertText('手机中文输入'); await phone.keyboard.press('Enter');
  await expect.poll(() => phone.evaluate(epoch => sessionStorage.getItem(`workbench-pending-input:${epoch}`), before.runEpoch)).toBeNull();
  await mobile.setOffline(true); await mobile.setOffline(false);
  await expect(phone.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  await phone.reload(); await expect(phone.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  for (const width of [390, 320]) {
    await phone.setViewportSize({ width, height: 844 });
    await expect.poll(() => phone.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await shot(phone, `phone-${width}`);
  }
  stage = 'revoke';
  await owner.locator('.wb-access-device').filter({ hasText: '手机浏览器' }).getByRole('button', { name: '断开', exact: true }).click();
  await expect.poll(() => phone.evaluate(async path => (await fetch(path)).status, runApiPath(phone.url(), '/run'))).toBe(401);
  assert.equal((await run(other)).processId, before.processId);
  await owner.getByRole('button', { name: '收起', exact: true }).click();
  await expect.poll(async () => await owner.locator('.wb-terminal').getAttribute('data-owned') === 'true' || await owner.getByRole('button', { name: '在此输入', exact: true }).isVisible()).toBe(true);
  if (await owner.getByRole('button', { name: '在此输入', exact: true }).isVisible()) await owner.getByRole('button', { name: '在此输入', exact: true }).click();
  await expect(owner.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await owner.locator('.xterm-helper-textarea').focus(); await owner.keyboard.insertText('电脑继续输入'); await owner.keyboard.press('Enter');
  await expect.poll(() => owner.evaluate(epoch => sessionStorage.getItem(`workbench-pending-input:${epoch}`), before.runEpoch)).toBeNull();
  assert.ok(await terminal.evaluate(e => e.isConnected)); assert.equal(sockets, 1);
  stage = 'owner-refresh'; await owner.reload();
  await expect(owner.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await owner.getByRole('button', { name: '手机接入', exact: true }).click();
  await expect(owner.locator('.wb-access-qr canvas')).toBeVisible(); await expect.poll(qr).toBe(ip);
  assert.equal((await run(owner)).processId, before.processId);
  stage = 'disable';
  await owner.getByRole('button', { name: '关闭设备访问', exact: true }).click();
  await expect(owner.getByRole('button', { name: '开启设备访问', exact: true })).toBeVisible();
  stage = 'reopen'; await owner.getByRole('button', { name: '开启设备访问', exact: true }).click();
  const reopenStarted = Date.now();
  await expect(owner.locator('.wb-access-qr canvas')).toBeVisible();
  await expect.poll(async () => new URL(await qr()).hash).toBe(new URL(ip).hash);
  const reopenToQrMs = Date.now() - reopenStarted;
  await expect(owner.locator('.wb-access-device')).toHaveCount(0);
  await owner.getByRole('button', { name: '关闭设备访问', exact: true }).click();
  await expect(owner.getByRole('button', { name: '开启设备访问', exact: true })).toBeVisible();
  assert.equal((await run(owner)).processId, before.processId); assert.equal(errors, 0);
  report({ stage: 'complete', chrome: browser.version(), errors, enableToQrMs, reopenToQrMs, checks: ['decoded-qr', 'fixed-code', 'repeat-scan-cookie', 'owner-refresh-same-qr', 'reopen-same-code', 'simultaneous-addresses', 'same-run-pid', 'non-localhost-http', 'mobile-stream', 'chinese-input', 'refresh', 'offline', '390', '320', 'revoke', 'local-preserved'] });
  await close(0);
})().catch(async error => { if (owner) await shot(owner, 'failure').catch(() => {}); report({ stage: 'failed', check: stage, line: error.stack?.match(/r5-device-access-probe\.cjs:(\d+)/)?.[1] }); close(1); });
