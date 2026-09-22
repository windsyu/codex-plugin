const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

async function screenshot(name) { if (page && process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${name}.png`, fullPage: true }); }
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 50000).unref();
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  page = await browser.newPage({ viewport: { width: 1600, height: 1000 } }); page.setDefaultTimeout(12000);
  let errors = 0; page.on('pageerror', () => errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  const input = page.locator('.xterm-helper-textarea'), terminal = page.locator('.wb-terminal');
  const screen = () => page.locator('.xterm-rows').innerText();
  const snapshot = () => page.evaluate(async () => (await fetch('/workbench/v1/live/snapshot')).json());
  const run = () => page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  await expect(terminal).toHaveAttribute('data-owned', 'true');
  stage = 'ready'; let themed = false, trusted = false, ready = false;
  for (let i = 0; i < 100; i++) {
    const value = await screen();
    if (!themed && /Choose your style|Select a theme/.test(value)) { await input.press('Enter'); themed = true; }
    else if (!trusted && /Do you trust|Do you want to work/.test(value)) { await input.press('Enter'); trusted = true; }
    else if (value.includes('OpenAI Codex') && value.includes('›')) { ready = true; break; }
    await page.waitForTimeout(120);
  }
  assert.ok(ready); const before = await run();
  const messages = page.locator('.wb-message-list'), users = messages.locator('[data-role="user"]'), models = messages.locator('[data-role="assistant"]');
  async function paste(value) {
    await input.evaluate((element, text) => { const transfer = new DataTransfer(); transfer.setData('text/plain', text); element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true })); }, value);
    await expect.poll(screen).toContain(value);
  }
  stage = 'first'; await paste('R2_STEER_START：请先开始回答，等待我的追加输入。'); await input.press('Enter');
  await expect(messages).toContainText('R2_STEER_PARTIAL'); await expect(users).toHaveCount(1); await expect(models).toHaveCount(1);
  const node = await models.first().elementHandle(), oldKey = (await snapshot()).items.find(item => item.kind === 'message' && item.author.role === 'assistant').itemKey;
  stage = 'draft'; await paste('R2_STEER_ADDED：这是流式回答期间提交的追加说明。');
  await expect(users).toHaveCount(1); // terminal draft is never an optimistic bubble
  const receiving = () => snapshot().then(value => value.responses.find(response => response.responseId === 'response-steering')?.status);
  assert.equal(await receiving(), 'receiving');
  stage = 'steer'; await input.press('Enter');
  await expect.poll(screen).toContain('R2_STEER_ADDED');
  assert.equal(await receiving(), 'receiving');
  await input.focus(); report({ stage: 'steer-submitted', firstResponseStillOpen: true, noDraftBubble: true });
  stage = 'complete-response'; await expect(messages).toContainText('R2_STEER_FINISHED'); await expect(users).toHaveCount(2); await expect(models).toHaveCount(2);
  await expect(messages).toContainText('R2_STEER_FIRST_COMPLETE');
  await expect(users.nth(1)).toContainText('R2_STEER_ADDED');
  await expect(users.nth(0)).toContainText('同轮多条提交'); await expect(users.nth(1)).toContainText('先后尚未确认');
  assert.equal(await node.evaluate(element => element === document.querySelector('.wb-message-list [data-role="assistant"]')), true);
  await expect(input).toBeFocused();
  const state = await snapshot(); const requests = state.requests.filter(request => request.purpose === 'conversation');
  assert.equal(requests.length, 2); assert.equal(requests[0].codexTurnId, requests[1].codexTurnId); assert.equal(requests[0].codexThreadId, requests[1].codexThreadId);
  assert.equal(state.items.find(item => item.itemKey === oldKey).content[0].text, 'R2_STEER_FIRST_COMPLETE：第一段回复结束。');
  const keys = state.items.map(item => item.itemKey);
  stage = 'refresh'; await page.reload(); await expect(terminal).toHaveAttribute('data-owned', 'true'); await expect(users).toHaveCount(2); await expect(models).toHaveCount(2);
  assert.deepEqual((await snapshot()).items.map(item => item.itemKey), keys);
  const after = await run(); assert.equal(before.processId, after.processId); assert.equal(before.runEpoch, after.runEpoch);
  assert.deepEqual((await snapshot()).userCapture.diagnostics, []); await screenshot('complete');
  assert.equal(errors, 0); await input.press('Control+d'); await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  report({ stage: 'complete', sameNativeTurn: true, userMessages: 2, modelMessages: 2, orderingUncertaintyVisible: true, stableModelNode: true, sameCliProcess: true, pageErrors: errors, browser: browser.version() }); await close(0);
})().catch(async () => { await screenshot('failed').catch(() => {}); report({ stage: 'failed', check: stage, reason: 'assertion (private URL/output suppressed)' }); close(1); });
