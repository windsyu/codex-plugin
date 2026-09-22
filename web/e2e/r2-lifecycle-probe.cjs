// Actual official CLI with synthetic policy/validation refusals and a cancelled stream.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

const screenshot = async suffix => { if (page && process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${suffix}.png`, fullPage: true }); };
const screen = () => page.locator('.xterm-rows').innerText();
const input = () => page.locator('.xterm-helper-textarea');
const snapshot = () => page.evaluate(async () => (await fetch('/workbench/v1/live/snapshot')).json());
async function submit(value) {
  await input().focus();
  await input().evaluate((element, value) => {
    const transfer = new DataTransfer(); transfer.setData('text/plain', value);
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  }, value);
  await expect.poll(screen).toContain(value); await input().press('Enter');
}
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 50000).unref();
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  page = await browser.newPage({ viewport: { width: 1600, height: 1000 } }); page.setDefaultTimeout(10000);
  let errors = 0; page.on('pageerror', () => errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  const beforeRun = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  let themed = false, trusted = false, ready = false;
  for (let n = 0; n < 100; n++) {
    const text = await screen();
    if (!themed && /Choose your style|Select a theme/.test(text)) { await input().press('Enter'); themed = true; }
    else if (!trusted && /Do you trust|Do you want to work/.test(text)) { await input().press('Enter'); trusted = true; }
    else if (text.includes('OpenAI Codex') && text.includes('›')) { ready = true; break; }
    await page.waitForTimeout(100);
  }
  assert.ok(ready); stage = 'native-refusal-results';
  await submit('R2_LIFECYCLE_REQUEST');
  const messages = page.locator('.wb-message-list');
  await expect(messages).toContainText('R2_LIFECYCLE_RESULTS_DONE');
  await expect(messages.locator('.wb-tool-card')).toHaveCount(3);
  for (const [id, text] of [[1, /reject|forbidden|policy/i], [2, 'future_fixture_tool'], [3, 'cmd']]) {
    const card = messages.locator(`[data-call-id="lifecycle-call-${id}"]`);
    await expect(card).toHaveAttribute('data-execution', 'result_observed');
    await card.locator('.wb-tool-result summary').press('Enter');
    await expect(card.locator('.wb-tool-result pre')).toContainText(text);
    await expect(card.locator('.wb-tool-result')).toContainText('后续模型请求');
  }
  await expect(messages.locator('[data-call-id="lifecycle-call-2"]')).toHaveAttribute('data-category', 'other');
  await expect(messages.locator('[data-role="user"]')).toHaveCount(1);
  await screenshot('refusals');
  stage = 'incomplete-parameters';
  await submit('R2_ABORT_PARAMETERS');
  const incomplete = messages.locator('[data-call-id="lifecycle-incomplete-call"]');
  await expect(incomplete).toContainText('R2_INCOMPLETE_PARAMETERS');
  await expect(incomplete).toContainText('参数生成中');
  await expect(incomplete).toHaveAttribute('data-execution', 'unobserved');
  await incomplete.locator('.wb-tool-arguments summary').press('Enter');
  const cardNode = await incomplete.elementHandle(), beforeCancel = await snapshot();
  const partialText = messages.locator('.wb-model-message').filter({ hasText: 'R5_CANCELLED_TEXT_PARTIAL' });
  await expect(partialText.locator('.wb-message-status')).toContainText('正在接收');
  await input().press('Escape');
  await expect(partialText.locator('.wb-message-status')).toContainText('本次响应不完整');
  await expect(incomplete).toContainText('参数不完整');
  await expect(incomplete).toHaveAttribute('data-execution', 'unobserved');
  await expect(incomplete.locator('.wb-tool-result')).toHaveCount(0);
  await expect(incomplete).not.toContainText('已取消');
  await expect(incomplete).not.toContainText('已拒绝执行');
  await expect(incomplete.locator('.wb-tool-arguments')).toHaveAttribute('open', '');
  assert.equal(await cardNode.evaluate(element => element.isConnected), true);
  await expect(input()).toBeFocused();
  const afterCancel = await snapshot();
  assert.deepEqual(afterCancel.items.filter(item => item.kind === 'tool_call').map(item => item.itemKey), beforeCancel.items.filter(item => item.kind === 'tool_call').map(item => item.itemKey));
  await screenshot('cancelled');
  stage = 'next-native-turn';
  await submit('R2_AFTER_ABORT');
  await expect(messages).toContainText('R2_AFTER_ABORT_DONE');
  await expect(messages.locator('.wb-tool-card')).toHaveCount(4);
  await expect(messages.locator('[data-role="user"]')).toHaveCount(3);
  await expect(messages.locator('.wb-model-message')).toHaveCount(3);
  await expect(incomplete).toContainText('参数不完整');
  await expect(incomplete).toHaveAttribute('data-execution', 'unobserved');
  const complete = await snapshot();
  assert.equal(complete.nativeCommands.length, 0); assert.equal(complete.nativeFileChanges.length, 0);
  assert.deepEqual(complete.userCapture.diagnostics, []);
  stage = 'reload'; await page.reload();
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(messages.locator('.wb-tool-card')).toHaveCount(4);
  await expect(messages.locator('[data-role="user"]')).toHaveCount(3);
  await expect(incomplete).toContainText('参数不完整');
  assert.deepEqual((await snapshot()).items.map(item => item.itemKey), complete.items.map(item => item.itemKey));
  const afterRun = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  assert.equal(afterRun.processId, beforeRun.processId); assert.equal(afterRun.runEpoch, beforeRun.runEpoch);
  await screenshot('complete');
  assert.equal(errors, 0);
  report({ stage: 'complete', nativeRefusalsKeepUnknownExecution: true, unsupportedToolReadable: true, partialCallNotMarkedCancelled: true, nextInputWorks: true, samePtyProcess: true, userMessages: 3, toolCards: 4, pageErrors: errors, browser: browser.version() });
  await close(0);
})().catch(async () => { await screenshot('failed').catch(() => {}); report({ stage: 'failed', check: stage, reason: 'assertion (private URL/output suppressed)' }); close(1); });
