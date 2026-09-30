// Synthetic R0 surface; uses installed Chrome and an isolated ephemeral profile.
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');

let browser, stage = 'launch';
const live = process.env.WORKBENCH_PROBE_MODE === 'live';
const headless = process.env.WORKBENCH_PROBE_HEADED !== '1';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const pngPath = prefix => prefix.endsWith('.png') ? prefix : `${prefix}.png`;

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, live ? 330000 : 35000).unref();

(async () => {
  browser = await chromium.launch({
    executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless,
  });
  const context = await browser.newContext({ viewport: { width: 1024, height: 740 } });
  const page = await context.newPage();
  page.setDefaultTimeout(live ? 300000 : 15000);
  let snapshots = 0, streams = 0, pageErrors = 0;
  page.on('pageerror', () => pageErrors++);
  page.on('request', request => {
    const path = new URL(request.url()).pathname;
    if (path === '/workbench/v1/live/snapshot') snapshots++;
    if (path === '/workbench/v1/live/events') streams++;
  });
  stage = 'pair';
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await page.waitForFunction(() => document.getElementById('connection').textContent === '已连接');
  assert.equal(new URL(page.url()).hash, '');
  const cookies = await context.cookies();
  assert.ok(cookies.some(cookie => cookie.httpOnly && cookie.sameSite === 'Strict'));
  assert.equal(snapshots, 1);
  report({ stage: 'ready' });

  if (live) {
    stage = 'live-intermediate';
    const handle = await page.waitForFunction(marker => {
      const card = [...document.querySelectorAll('#messages article')].find(article =>
        article.querySelector('.text').textContent.includes(marker) && article.querySelector('.text').textContent.length >= 160 &&
        article.querySelector('.response-status').textContent.includes('接收中'));
      return card ? { requestId: card.dataset.requestId, characters: card.querySelector('.text').textContent.length, viewSeq: document.body.dataset.viewSeq } : false;
    }, process.env.WORKBENCH_PROBE_PARTIAL);
    const intermediate = await handle.jsonValue();
    report({ stage: 'intermediate', ...intermediate, snapshots, streams });
    stage = 'live-final';
    await page.waitForFunction(id => [...document.querySelectorAll('#messages article')].some(article =>
      article.dataset.requestId === id && article.querySelector('.response-status').textContent.includes('本次模型响应已结束')), intermediate.requestId);
    assert.equal(snapshots, 1);
    assert.equal(streams, 1);
    assert.equal(pageErrors, 0);
    report({ stage: 'final', requestId: intermediate.requestId, snapshots, streams });
    stage = 'live-refresh';
    await page.reload();
    await page.waitForFunction(() => document.getElementById('connection').textContent === '已连接');
    assert.ok(await page.locator(`article[data-request-id="${intermediate.requestId}"] .text`).count());
    if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: pngPath(process.env.WORKBENCH_PROBE_SCREENSHOT), fullPage: true });
    report({ stage: 'complete', requestId: intermediate.requestId, snapshots, streams, pageErrors, browser: browser.version(), headless });
    await close(0);
    return;
  }

  stage = 'intermediate';
  const partial = process.env.WORKBENCH_PROBE_PARTIAL;
  const finalText = process.env.WORKBENCH_PROBE_FINAL;
  await page.waitForFunction(value => [...document.querySelectorAll('.text')].some(node => node.textContent.includes(value)), partial);
  assert.ok((await page.locator('.response-status').first().textContent()).includes('接收中'));
  assert.equal(snapshots, 1, 'intermediate events must not trigger repeated snapshots');
  assert.equal(streams, 1);
  report({ stage: 'intermediate', snapshots, streams });

  stage = 'final';
  await page.waitForFunction(value => [...document.querySelectorAll('.text')].some(node => node.textContent === value), finalText);
  await page.waitForFunction(() => [...document.querySelectorAll('.response-status')].some(node => node.textContent.includes('本次模型响应已结束')));
  assert.equal(snapshots, 1, 'streaming must not refetch each delta or completion');
  assert.equal(streams, 1);
  assert.equal(pageErrors, 0);
  if (process.env.WORKBENCH_PROBE_MODE === 'reading') {
    assert.equal(await page.locator('#messages article').count(), 1);
    assert.equal(await page.locator('#messages img, #messages script, #messages iframe').count(), 0);
    assert.equal(await page.evaluate(() => window.r0Injected), undefined);
    assert.ok(!(await page.locator('body').textContent()).includes('fixture-sensitive-value'));
    assert.ok((await page.locator('footer').textContent()).includes('未启用'));
    assert.ok((await page.locator('.response-status').textContent()).includes('不代表任务结束'));
  }
  report({ stage: 'final', snapshots, streams });

  stage = 'refresh';
  await page.reload();
  await page.waitForFunction(() => document.getElementById('connection').textContent === '已连接');
  assert.equal(await page.locator('.text').filter({ hasText: finalText }).textContent(), finalText);
  assert.equal(snapshots, 2);
  assert.equal(streams, 2);
  assert.equal(new URL(page.url()).hash, '');
  for (const width of [1024, 736, 320]) {
    stage = `width-${width}`;
    await page.setViewportSize({ width, height: 740 });
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
  }
  await page.setViewportSize({ width: 1024, height: 740 });
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) {
    stage = 'screenshot';
    await page.screenshot({ path: pngPath(process.env.WORKBENCH_PROBE_SCREENSHOT), fullPage: true });
  }
  report({ stage: 'complete', snapshots, streams, pageErrors, browser: browser.version(), headless });
  await close(0);
})().catch(() => { report({ stage: 'failed', check: stage }); close(1); });
