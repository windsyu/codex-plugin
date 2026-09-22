const { openCalls, closeCalls } = require('./call-inspector.cjs');
// Synthetic decoder/PTY fixture, using the real workbench UI in installed Chrome.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 30000).unref();
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } }); page.setDefaultTimeout(10000);
  let errors = 0, snapshotReads = 0; page.on('pageerror', () => errors++);
  page.on('request', request => { if (new URL(request.url()).pathname === '/workbench/v1/live/snapshot') snapshotReads++; });
  await page.addInitScript(() => {
    const NativeEventSource = window.EventSource;
    window.dropNextReadingTextPatch = false;
    window.droppedReadingTextPatch = false;
    window.EventSource = class extends NativeEventSource {
      addEventListener(type, listener, options) {
        if (type !== 'view') return super.addEventListener(type, listener, options);
        return super.addEventListener(type, event => {
          const value = JSON.parse(event.data);
          if (window.dropNextReadingTextPatch && value.kind === 'item.patch' && value.field === 'text') {
            window.dropNextReadingTextPatch = false; window.droppedReadingTextPatch = true; return;
          }
          listener.call(this, event);
        }, options);
      }
    };
  });
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  report({ stage: 'ready' }); stage = 'partial';
  const messages = page.locator('.wb-message-list');
  const model = messages.locator('.wb-model-message');
  const tool = messages.locator('.wb-tool-card');
  await expect(model).toHaveCount(1); await expect(tool).toHaveCount(1);
  await expect(model).toContainText('R2_IDENTITY_PARTIAL');
  await expect(tool).toContainText('echo partial');
  const modelNode = await model.elementHandle(), toolNode = await tool.elementHandle();
  const snapshotBefore = await page.evaluate(async () => (await fetch('/workbench/v1/live/snapshot')).json());
  const runBefore = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  await tool.locator('summary').first().press('Enter');
  await page.locator('.xterm-helper-textarea').focus();
  const readsBeforeGap = snapshotReads;
  await page.evaluate(() => { window.dropNextReadingTextPatch = true; });
  report({ stage: 'partial' }); stage = 'gap-recovery';
  await expect(model).toContainText('SNAPSHOT_RECOVERY');
  await expect(model).toContainText('fixture-reported');
  assert.equal(await page.evaluate(() => window.droppedReadingTextPatch), true);
  assert.ok(snapshotReads > readsBeforeGap);
  assert.equal(await modelNode.evaluate(element => element === document.querySelector('.wb-message-list .wb-model-message')), true);
  await expect(tool.locator('details').first()).toHaveAttribute('open', '');
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  report({ stage: 'recovered' }); stage = 'final';
  await expect(model).toContainText('R2_IDENTITY_FINAL');
  await expect(tool).toContainText('echo final');
  await expect(model).toHaveCount(1); await expect(tool).toHaveCount(1);
  assert.equal(await modelNode.evaluate(element => element === document.querySelector('.wb-message-list .wb-model-message')), true);
  assert.equal(await toolNode.evaluate(element => element === document.querySelector('.wb-message-list .wb-tool-card')), true);
  await expect(tool.locator('details').first()).toHaveAttribute('open', '');
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  await expect(tool).toContainText('尚未观察到执行');
  const snapshotAfter = await page.evaluate(async () => (await fetch('/workbench/v1/live/snapshot')).json());
  assert.equal(snapshotAfter.schemaVersion, 2);
  assert.equal(snapshotAfter.tools, undefined); assert.equal(snapshotAfter.userMessages, undefined);
  assert.deepEqual(snapshotAfter.items.map(item => item.itemKey), snapshotBefore.items.map(item => item.itemKey));
  assert.deepEqual(snapshotAfter.items.map(item => item.kind), ['message', 'tool_call']);
  await openCalls(page);
  await closeCalls(page);
  await expect(tool.locator('details').first()).toHaveAttribute('open', '');
  stage = 'refresh';
  await page.route('**/workbench/v1/live/snapshot', async route => {
    const response = await route.fetch(); const value = await response.json();
    value.items.push({ kind: 'future_unrecognized', itemKey: 'future:item', revision: 1, orderIndex: value.viewSeq + 1, payload: '<img src=x onerror="window.unknownExecuted=1">PRIVATE_UNKNOWN_PAYLOAD' });
    await route.fulfill({ response, json: value });
  });
  await page.reload();
  await expect(messages.locator('.wb-notice')).toContainText('未识别的内容类型');
  assert.ok(!(await messages.textContent()).includes('PRIVATE_UNKNOWN_PAYLOAD'));
  assert.equal(await page.evaluate(() => window.unknownExecuted), undefined);
  await expect(model).toHaveCount(1); await expect(tool).toHaveCount(1);
  await expect(model).toContainText('R2_IDENTITY_FINAL'); await expect(tool).toContainText('echo final');
  const runAfter = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  assert.equal(runAfter.processId, runBefore.processId); assert.equal(runAfter.runEpoch, runBefore.runEpoch);
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.identity.png`, fullPage: true });
  assert.equal(errors, 0);
  report({ stage: 'complete', stableModelNode: true, stableToolNode: true, samePtyProcess: true, snapshotGapRecovery: true, unknownTypeSafeNotice: true, schemaVersion: 2, pageErrors: errors, browser: browser.version() });
  await close(0);
})().catch(() => { report({ stage: 'failed', check: stage, reason: 'assertion (private URL/output suppressed)' }); close(1); });
