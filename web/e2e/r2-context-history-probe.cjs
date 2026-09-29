const { readRun } = require('./workbench-api.cjs');
const { openCalls, closeCalls } = require('./call-inspector.cjs');
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const resumed = process.env.WORKBENCH_PROBE_RESUMED === 'true';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

async function screenshot(name) { if (page && process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${resumed ? 'resume' : 'fork'}.${name}.png`, fullPage: true }); }
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 60000).unref();
const PROMPT = 'R2_CONTEXT_INPUT：同一句提交，保留合成上下文。';
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  page = await browser.newPage({ viewport: { width: 1600, height: 1000 } }); page.setDefaultTimeout(12000);
  let errors = 0; page.on('pageerror', () => errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  const terminal = page.locator('.wb-terminal'), input = page.locator('.xterm-helper-textarea');
  const screen = () => page.locator('.xterm-rows').innerText();
  const snapshot = () => readRun(page, '/live/snapshot');
  const run = () => readRun(page, '/run');
  await expect(terminal).toHaveAttribute('data-owned', 'true');
  stage = 'ready'; let modelNoticeHandled = false;
  let themed = false, trusted = false, ready = false;
  for (let i = 0; i < 100; i++) {
    const value = await screen();
    if (value.includes('Try new model') && value.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await input.press('ArrowDown'); await input.press('Enter');
      }
    } else if (!themed && /Choose your style|Select a theme/.test(value)) { await input.press('Enter'); themed = true; }
    else if (!trusted && /Do you trust|Do you want to work|Trust this folder\?/.test(value)) { await input.press('Enter'); trusted = true; }
    else if (value.includes('OpenAI Codex') && value.includes('›')) { ready = true; break; }
    await page.waitForTimeout(120);
  }
  assert.ok(ready); const before = await run();
  const messages = page.locator('.wb-message-list');
  const users = messages.locator('[data-role="user"]'), models = messages.locator('[data-role="assistant"]');
  if (resumed) await expect.poll(screen).toContain('R2_CONTEXT_REPLY_3');
  await page.waitForTimeout(400);
  assert.equal((await snapshot()).requests.filter(request => request.purpose === 'conversation').length, 0);
  await expect(users).toHaveCount(0);
  async function submit(value) {
    await input.evaluate((element, text) => { const transfer = new DataTransfer(); transfer.setData('text/plain', text); element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true })); }, value);
    await expect.poll(screen).toContain(value); await input.press('Enter');
  }
  async function reply(index, count) {
    await expect(messages).toContainText(`R2_CONTEXT_REPLY_${index}`);
    await expect(users).toHaveCount(count); await expect(models).toHaveCount(count);
    await expect.poll(screen).toContain(`R2_CONTEXT_REPLY_${index}`);
  }
  stage = 'first'; await submit(PROMPT); await reply(resumed ? 4 : 1, 1);
  const firstModel = await models.first().elementHandle();
  if (!resumed) {
    stage = 'compact'; await submit('/compact');
    await expect.poll(screen).toContain('Context compacted');
    await expect(users).toHaveCount(1); await expect(models).toHaveCount(1);
    assert.ok(!(await messages.textContent()).includes('R2_COMPACT_SUMMARY'));
    await openCalls(page);
    // The compact list deliberately omits bodies; select the captured compaction
    // by its public snapshot identity, keeping title requests in the same list.
    const compactRequest = (await snapshot()).items.find(item => item.kind === 'message' && item.content.some(part => part.text.includes('R2_COMPACT_SUMMARY'))).evidence[0].key.requestId;
    const auxiliary = page.locator(`.wb-call-row[data-purpose="auxiliary"][data-request-id="${compactRequest}"]`);
    await expect(auxiliary).toHaveCount(1);
    await auxiliary.click();
    await page.locator('.wb-call-unassigned > summary').click();
    await expect(page.locator('.wb-call-inspector')).toContainText('R2_COMPACT_SUMMARY');
    assert.equal(await page.evaluate(() => window.contextInjected), undefined);
    await closeCalls(page);
    stage = 'repeat-after-compact'; await submit(PROMPT); await reply(2, 2);
    await expect(users.nth(0)).toContainText(PROMPT); await expect(users.nth(1)).toContainText(PROMPT);
    stage = 'fork'; await submit('/fork');
    await expect.poll(screen).toContain('To continue this session');
    await expect(users).toHaveCount(2); await expect(models).toHaveCount(2);
    stage = 'after-fork'; await submit(PROMPT); await reply(3, 3);
    const state = await snapshot(); const requests = state.requests.filter(request => request.purpose === 'conversation');
    assert.equal(requests.length, 3); assert.equal(requests[0].codexThreadId, requests[1].codexThreadId); assert.notEqual(requests[1].codexThreadId, requests[2].codexThreadId);
    assert.equal(await firstModel.evaluate(element => element === document.querySelector('.wb-message-list [data-role="assistant"]')), true);
  }
  stage = 'context-details'; await models.last().getByRole('button', { name: '调用详情', exact: true }).click();
  const last = page.locator('.wb-call-inspector');
  await expect(last).toContainText('R2_COMPACT_SUMMARY');
  assert.equal(await page.evaluate(() => window.contextInjected), undefined);
  await screenshot('context');
  await closeCalls(page);
  stage = 'refresh'; const keys = (await snapshot()).items.map(item => item.itemKey); await page.reload();
  await expect(terminal).toHaveAttribute('data-owned', 'true');
  await expect(users).toHaveCount(resumed ? 1 : 3); await expect(models).toHaveCount(resumed ? 1 : 3);
  assert.deepEqual((await snapshot()).items.map(item => item.itemKey), keys);
  const after = await run(); assert.equal(before.processId, after.processId); assert.equal(before.runEpoch, after.runEpoch);
  assert.deepEqual((await snapshot()).userCapture.diagnostics, []);
  await screenshot('complete');
  assert.equal(errors, 0); await input.press('Control+d'); await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  report({ stage: 'complete', resumed, sameCliProcess: true, noAutomaticReplay: true, userMessages: resumed ? 1 : 3, modelMessages: resumed ? 1 : 3, summaryOnlyInRequests: true, pageErrors: errors, browser: browser.version() });
  await close(0);
})().catch(async () => { await screenshot('failed').catch(() => {}); report({ stage: 'failed', check: stage, resumed, reason: 'assertion (private URL/output suppressed)' }); close(1); });
