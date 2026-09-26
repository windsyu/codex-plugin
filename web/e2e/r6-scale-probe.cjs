// Synthetic history only. Reports UI readiness separately from catalog coverage.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const { execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
let stage = 'launch', timer;
const report = value => process.stdout.write(JSON.stringify(value) + '\n');
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 480000).unref();
(async () => {
  const expected = Number(process.env.WORKBENCH_PROBE_SESSIONS);
  const browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });
  const page = await context.newPage(); page.setDefaultTimeout(8000);
  const cdp = await context.newCDPSession(page); await cdp.send('Performance.enable');
  let peakHeap = 0, peakDom = 0, peakRssKiB = 0, peakCpuPercent = 0, samples = 0, errors = 0, external = 0, sockets = 0;
  page.on('pageerror', () => errors++); page.on('websocket', () => sockets++);
  page.on('request', request => { if (new URL(request.url()).origin !== new URL(process.env.WORKBENCH_PROBE_URL).origin) external++; });
  let sampling = false;
  async function sample() {
    if (sampling) return; sampling = true;
    try {
      const values = new Map((await cdp.send('Performance.getMetrics')).metrics.map(v => [v.name, v.value]));
      peakHeap = Math.max(peakHeap, values.get('JSHeapUsedSize') || 0);
      peakDom = Math.max(peakDom, (await cdp.send('Memory.getDOMCounters')).nodes);
      const [rss, cpu] = execFileSync('ps', ['-o', 'rss=,%cpu=', '-p', process.env.WORKBENCH_PROBE_PID], { encoding: 'utf8' }).trim().split(/\s+/).map(Number);
      peakRssKiB = Math.max(peakRssKiB, rss); peakCpuPercent = Math.max(peakCpuPercent, cpu); samples++;
    } finally { sampling = false; }
  }
  stage = 'cold-interaction';
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  await page.getByRole('button', { name: '设置', exact: true }).click();
  await expect(page.locator('#wb-settings-panel')).toBeVisible();
  const cold = await page.evaluate(() => ({
    navigationToInteractionMs: performance.now(),
    ttfbMs: performance.getEntriesByType('navigation')[0].responseStart,
    firstContentfulPaintMs: performance.getEntriesByName('first-contentful-paint')[0]?.startTime ?? null,
  }));
  await page.keyboard.press('Escape');
  report({ stage: 'cold-ui', ...cold });
  await sample();
  timer = setInterval(() => { void sample().catch(() => {}); }, 500);
  stage = 'cold-publication';
  let terminal, firstRecordMs = null;
  const deadline = Date.now() + 420000;
  while (Date.now() < deadline) {
    terminal = await page.evaluate(async () => (await fetch('/workbench/v1/application')).json());
    assert.equal(terminal.runs.length, 0);
    if (firstRecordMs === null && await page.locator('.wb-library-entry').count()) firstRecordMs = await page.evaluate(() => performance.now());
    const source = terminal.sources.find(s => s.id === 'default-native');
    if (source && source.state !== 'indexing') break;
    await page.waitForTimeout(500);
  }
  const source = terminal.sources.find(s => s.id === 'default-native');
  report({ stage: 'catalog', expected, indexed: source?.indexedEntries, state: source?.state, firstRecordMs, publishedMs: await page.evaluate(() => performance.now()) });
  assert.ok(['ready', 'partial'].includes(source?.state) && typeof source.revision === 'string', 'catalog must publish a revision');
  assert.equal(source?.indexedEntries, expected, 'catalog must include every synthetic session');
  stage = 'automatic-first-list';
  await expect(page.locator('.wb-library-entry')).toHaveCount(Math.min(expected, 30));
  if (firstRecordMs === null) firstRecordMs = await page.evaluate(() => performance.now());
  report({ stage: 'first-list', navigationToVisibleListMs: firstRecordMs, trigger: 'automatic' });
  stage = 'warm-list';
  const mounted = await page.locator('.wb-library-entry').count();
  const reads = await page.evaluate(async expectedRows => {
    const measurements = [];
    const assertPage = body => {
      if (!Array.isArray(body.records) || body.records.length !== expectedRows || typeof body.revision !== 'string' || !body.records.every(record => typeof record.entryId === 'string')) throw new Error('incomplete warm metadata page');
    };
    for (let i = 0; i < 30; i++) {
      const start = performance.now();
      const response = await fetch('/workbench/v1/library/entries?limit=30');
      if (!response.ok) throw new Error('warm list failed');
      const body = await response.json();
      assertPage(body); measurements.push(performance.now() - start);
    }
    return measurements.sort((a, b) => a - b);
  }, Math.min(expected, 30));
  await page.getByRole('button', { name: '更新历史', exact: true }).click();
  stage = 'reading';
  await page.locator('.wb-library-entry').first().click();
  await expect(page.locator('.wb-library-reader')).toBeVisible();
  await expect(page.locator('.wb-library-record').filter({ hasText: /g1s[0-9]{5}i[0-9]{2}/ }).first()).toBeVisible();
  await expect(page.locator('.wb-library-reader [role=alert]')).toHaveCount(0);
  await sample();
  clearInterval(timer);
  const warmP95Ms = reads[Math.ceil(reads.length * 0.95) - 1];
  report({ stage: 'measurements', sessions: expected, cold, warmHttp: { n: reads.length, p50Ms: reads[14], p95Ms: warmP95Ms, maxMs: reads.at(-1) }, resources: { sampleIntervalMs: 500, samples, peakHeapBytes: peakHeap, peakDomNodes: peakDom, serverPeakRssKiB: peakRssKiB, serverPeakSampledCpuPercent: peakCpuPercent, cpuDefinition: 'maximum sampled ps process-average %cpu, not interval CPU' }, mountedEntries: mounted, pageErrors: errors, externalRequests: external, webSockets: sockets, chrome: browser.version() });
  assert.ok(cold.navigationToInteractionMs <= 1000, 'cold page interaction must complete within 1s');
  assert.ok(warmP95Ms <= 200, 'warm metadata HTTP p95 must stay within 200ms');
  assert.ok(mounted <= 30); assert.equal(errors, 0); assert.equal(external, 0); assert.equal(sockets, 0);
  report({ stage: 'passed', check: 'r6-scale-browser' });
  await close(0);
})().catch(() => { clearInterval(timer); report({ stage: 'failed', check: stage }); close(1); });
