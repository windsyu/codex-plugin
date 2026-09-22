const { openCalls, closeCalls } = require('./call-inspector.cjs');
// Real CLI, synthetic model and project; no real credentials or screenshots of user work.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const polled = process.env.WORKBENCH_PROBE_POLLED === 'true';
const longOutput = process.env.WORKBENCH_PROBE_LONG_OUTPUT === 'true';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 45000).unref();
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  page = await browser.newPage({ viewport: { width: 1440, height: 900 } }); page.setDefaultTimeout(12000);
  let errors = 0; page.on('pageerror', () => errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  const input = page.locator('.xterm-helper-textarea');
  const screen = () => page.locator('.xterm-rows').innerText();
  stage = 'native-ready'; let themed = false, trusted = false, ready = false;
  for (let n = 0; n < 100; n++) {
    const text = await screen();
    if (!themed && /Choose your style|Select a theme/.test(text)) { await input.press('Enter'); themed = true; }
    else if (!trusted && /Do you trust|Do you want to work/.test(text)) { await input.press('Enter'); trusted = true; }
    else if (text.includes('OpenAI Codex') && text.includes('›')) { ready = true; break; }
    await page.waitForTimeout(100);
  }
  assert.ok(ready);
  const before = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  const prompt = 'R2_NATIVE_RESULT：验证原生退出记录。';
  await input.evaluate((element, value) => { const transfer = new DataTransfer(); transfer.setData('text/plain', value); element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true })); }, prompt);
  await expect.poll(screen).toContain(prompt); await input.press('Enter');
  const messages = page.locator('.wb-message-list');
  const card = messages.locator('[data-call-id="call_browser_native"]');
  stage = polled ? 'running-during-native-poll' : 'running-after-model-response';
  if (!polled) await expect(messages).toContainText('R2_BROWSER_MODEL_FINISHED');
  await expect(card).toHaveAttribute('data-execution', 'running');
  await expect(messages.locator('.wb-tool-card')).toHaveCount(polled ? 2 : 1);
  const node = await card.elementHandle();
  await card.locator('summary').first().press('Enter');
  report({ stage: 'running', polled, modelFinishedBeforeCommand: !polled });
  stage = 'late-native-result';
  await expect(card).toHaveAttribute('data-execution', 'failed');
  await expect(messages).toContainText('R2_BROWSER_MODEL_FINISHED');
  await expect(card.locator('.wb-tool-result')).toContainText('原生运行记录');
  await expect(card.locator('.wb-tool-result')).toContainText('R2_BROWSER_NATIVE_FINAL');
  await expect(card).toContainText('退出码：7');
  assert.equal(await node.evaluate(element => element === document.querySelector('.wb-message-list [data-call-id="call_browser_native"]')), true);
  await expect(card.locator('details').first()).toHaveAttribute('open', '');
  await expect(card.locator('.wb-tool-result details')).toHaveAttribute('open', '');
  if (longOutput) {
    stage = 'bounded-safe-native-output';
    await expect(card).toContainText('输出预览超限或来源含截断标记');
    await expect(card.locator('.wb-tool-result pre')).toContainText('<svg onload=globalThis.r2Unsafe=1>literal</svg>');
    assert.equal(await card.locator('svg,script').count(), 0);
    assert.equal(await page.evaluate(() => globalThis.r2Unsafe), undefined);
    const text = await card.locator('.wb-tool-result pre').textContent();
    assert.ok(Buffer.byteLength(text, 'utf8') <= 65536);
    assert.ok(text.includes('[已脱敏]'));
    assert.equal(await card.locator('.wb-tool-result pre').evaluate(element => element.scrollHeight > element.clientHeight && ['auto', 'scroll'].includes(getComputedStyle(element).overflowY)), true);
    assert.equal(await page.evaluate(async () => {
      const data = JSON.stringify(await (await fetch('/workbench/v1/live/snapshot')).json());
      return data.includes('r2-output-credential') || data.includes('synthetic-field-value');
    }), false);
  }
  stage = 'refresh'; await page.reload();
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(card).toHaveAttribute('data-execution', 'failed');
  await expect(messages.locator('.wb-tool-card')).toHaveCount(polled ? 2 : 1);
  const after = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  assert.equal(after.processId, before.processId); assert.equal(after.runEpoch, before.runEpoch);
  await card.getByRole('button', { name: '调用详情', exact: true }).click();
  const records = page.locator('.wb-native-commands').first();
  await expect(records).toBeVisible(); await records.locator('summary').press('Enter');
  await expect(records).toContainText('R2_BROWSER_NATIVE_FINAL');
  for (const width of [1024, 736, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
  await page.setViewportSize({ width: 1440, height: 900 });
  await closeCalls(page);
  await messages.evaluate(element => { element.scrollTop = 0; });
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${longOutput ? 'long' : polled ? 'polled' : 'late'}.png`, fullPage: true });
  assert.equal(errors, 0); await input.press('Control+d');
  await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  report({ stage: 'complete', polled, longOutput, originalCardUpdated: true, sameCliProcess: true, pageErrors: errors, browser: browser.version() });
  await close(0);
})().catch(async () => {
  if (page && process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.failed.png`, fullPage: true }).catch(() => {});
  report({ stage: 'failed', check: stage, polled, reason: 'assertion (private URL/output suppressed)' }); close(1);
});
