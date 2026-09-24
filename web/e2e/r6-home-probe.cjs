const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let stage = 'launch';
setTimeout(() => { process.stdout.write(JSON.stringify({ stage: 'failed', check: stage }) + '\n'); close(1); }, 45000).unref();
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  page.setDefaultTimeout(6000);
  let sockets = 0, errors = 0; const paths = [];
  page.on('websocket', () => sockets++); page.on('pageerror', () => errors++);
  page.on('request', request => { paths.push(new URL(request.url()).pathname); });
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  stage = 'home';
  await expect(page.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  await expect(page.locator('.wb-home-sources')).toContainText('无法读取历史位置');
  assert.equal(new URL(page.url()).hash, '');
  const instance = await page.evaluate(async () => (await (await fetch('/workbench/v1/application')).json()).instanceId);
  for (const width of [1440, 736, 390, 320]) {
    stage = `viewport-${width}`;
    await page.setViewportSize({ width, height: 1000 });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    await page.getByRole('button', { name: '设置', exact: true }).click();
    const panel = page.getByRole('region', { name: '工作台设置', exact: true });
    await expect(panel).toBeVisible();
    await expect(panel.getByLabel('允许清理历史记录', { exact: true })).not.toBeChecked();
    await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeInViewport();
    await expect(panel).not.toContainText('本次运行日志的位置');
    if (width === 1440 && process.env.WORKBENCH_TEST_HISTORY_SOURCE) {
      stage = 'register-history';
      await panel.getByRole('button', { name: '接入旧历史', exact: true }).click();
      await panel.getByLabel('历史类型', { exact: true }).selectOption('native');
      await panel.getByLabel('历史文件位置', { exact: true }).fill(process.env.WORKBENCH_TEST_HISTORY_SOURCE);
      await panel.getByRole('button', { name: '加入待保存来源', exact: true }).click();
      const draftState = await page.evaluate(async () => (await (await fetch('/workbench/v1/library/entries')).json()).records);
      assert.equal(draftState.length, 0);
      await panel.getByRole('button', { name: '保存', exact: true }).click();
      await expect(panel.getByRole('button', { name: '保存', exact: true })).toBeDisabled();
      await expect.poll(async () => await page.evaluate(async () => (await (await fetch('/workbench/v1/library/entries?q=' + encodeURIComponent('浏览器接入'))).json()).records.length)).toBe(1);
      await expect(page.locator('.wb-home-sources')).toContainText('已索引 1 条记录');
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
      stage = 'registered-history';
    }
    if (process.env.WORKBENCH_PROBE_SCREENSHOT && [1440, 390].includes(width)) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.settings-${width}.png` });
    await panel.getByRole('button', { name: '收起', exact: true }).press('Escape');
    await expect(panel).toBeHidden();
    await expect(page.getByRole('button', { name: '设置', exact: true })).toBeFocused();
  }
  stage = 'refresh';
  await page.reload();
  await expect(page.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  assert.equal(instance, await page.evaluate(async () => (await (await fetch('/workbench/v1/application')).json()).instanceId));
  await page.setViewportSize({ width: 1440, height: 1000 });
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.home.png`, fullPage: true });
  assert.equal(sockets, 0); assert.equal(errors, 0);
  assert.ok(!paths.some(path => /\/terminal|\/live\/|\/run$/.test(path)));
  process.stdout.write(JSON.stringify({ stage: 'passed', viewports: 4, webSockets: sockets, pageErrors: errors }) + '\n');
  await close(0);
})().catch(() => { process.stdout.write(JSON.stringify({ stage: 'failed', check: stage }) + '\n'); close(1); });
