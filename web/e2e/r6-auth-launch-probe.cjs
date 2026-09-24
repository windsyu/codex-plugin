// Isolated real CLI regression: requires_openai_auth=true with explicit provider bearer.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let stage = 'home', page;
setTimeout(() => { process.stdout.write(JSON.stringify({ stage: 'failed', check: stage, reason: 'deadline' }) + '\n'); close(1); }, 95000).unref();
const terminal = () => page.locator('.xterm-helper-textarea');
const screen = () => page.locator('.xterm-rows').innerText();
const app = () => page.evaluate(async () => (await fetch('/workbench/v1/application')).json());
const snapshot = id => page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/live/snapshot`)).json(), id);
function redacted(value) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  assert.ok(!text.includes('synthetic-r6-launch'), 'provider bearer leaked into live observation');
  assert.ok(!text.includes('synthetic-ambient-api-key'), 'ambient API key leaked into live observation');
}
async function nativeReady() {
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  for (let n = 0; n < 100; n++) {
    const text = await screen();
    if (/Choose your style|Select a theme|Do you trust|Do you want to work|Trust this folder/.test(text)) await terminal().press('Enter');
    else if (text.includes('OpenAI Codex') && text.includes('›')) return;
    await page.waitForTimeout(150);
  }
  throw new Error('native not ready');
}
async function paste(value) {
  await terminal().evaluate((el, value) => { const data = new DataTransfer(); data.setData('text/plain', value); el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true })); }, value);
}
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.setDefaultTimeout(15000);
  let errors = 0, starts = 0;
  page.on('pageerror', () => errors++);
  page.on('request', request => { if (new URL(request.url()).pathname === '/workbench/v1/runs' && request.method() === 'POST') starts++; });
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  assert.equal((await app()).runs.length, 0);
  stage = 'new-absolute-project';
  await page.getByRole('button', { name: '输入路径', exact: true }).click();
  await page.getByLabel('项目目录', { exact: true }).fill(process.env.WORKBENCH_PROBE_PROJECT);
  await page.getByLabel('项目目录', { exact: true }).press('Enter');
  await page.getByRole('button', { name: '开始新对话', exact: true }).click();
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  const first = (await app()).runs[0];
  assert.equal(first.projectPath, process.env.WORKBENCH_PROBE_PROJECT);
  assert.equal(new URL(page.url()).searchParams.get('run'), first.runId);
  await nativeReady();
  stage = 'live-redaction-before-completion';
  await paste('R6_E_FIRST：请记住本次合成会话。');
  await terminal().press('Enter');
  const messages = page.locator('.wb-message-list');
  await expect(messages).toContainText('R6_E_REDACTED');
  stage = 'intermediate-reply-timing';
  const intermediate = await messages.innerText();
  assert.ok(!intermediate.includes('R6_E_STREAM_DONE'), 'intermediate reply arrived only after completion');
  stage = 'intermediate-ui-redaction';
  redacted(intermediate);
  stage = 'intermediate-snapshot';
  const live = await snapshot(first.runId);
  assert.ok(JSON.stringify(live).includes('R6_E_REDACTED'));
  stage = 'intermediate-snapshot-redaction';
  redacted(live);
  stage = 'stream-completion';
  await expect(messages).toContainText('R6_E_STREAM_DONE');
  await expect.poll(screen).toContain('R6_E_STREAM_DONE');
  stage = 'files-in-selected-project';
  await page.getByRole('button', { name: '文件', exact: true }).click();
  await page.getByRole('treeitem').filter({ hasText: 'R6-check.txt' }).click();
  await expect(page.getByRole('region', { name: '代码内容', exact: true })).toContainText('Synthetic R6 source');
  stage = 'stop-and-explicit-resume';
  await terminal().press('Control+d');
  await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  await page.getByRole('dialog', { name: '本次运行已结束', exact: true }).getByRole('link', { name: '返回首页', exact: true }).click();
  await page.getByRole('button', { name: '更新历史', exact: true }).click();
  await expect.poll(async () => page.evaluate(async () => (await (await fetch('/workbench/v1/library/entries?kind=native')).json()).records.length)).toBe(1);
  await page.getByLabel('历史来源筛选').selectOption('default-native');
  await expect(page.locator('.wb-library-entry')).toHaveCount(1);
  await page.locator('.wb-library-entry').click();
  await page.getByRole('button', { name: '继续此会话', exact: true }).click();
  const panel = page.getByRole('dialog', { name: '继续此会话', exact: true });
  await panel.getByRole('button', { name: '继续此会话', exact: true }).click();
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  await nativeReady();
  await expect.poll(screen).toContain('R6_E_HISTORY');
  const second = (await app()).runs[0];
  assert.notEqual(second.runId, first.runId);
  assert.notEqual(second.cliPid, first.cliPid);
  assert.ok(second.resume);
  assert.equal(starts, 2);
  const beforeInput = await snapshot(second.runId);
  assert.equal(beforeInput.requests.filter(request => request.purpose === 'conversation').length, 0);
  redacted(beforeInput);
  await paste('R6_E_NEXT：继续刚才的会话。');
  await terminal().press('Enter');
  await expect(messages).toContainText('R6_E_DONE');
  redacted(await snapshot(second.runId));
  assert.equal(errors, 0);
  process.stdout.write(JSON.stringify({ stage: 'passed', starts, differentProjectCwd: true, intermediateBeforeCompletion: true, liveSecretsRedacted: true, sameNativeThread: true, filesRead: true, pageErrors: errors, browser: browser.version() }) + '\n');
  await close(0);
})().catch(async () => {
  // Never print terminal text, request bodies, pairing URLs or browser exception details.
  process.stdout.write(JSON.stringify({ stage: 'failed', check: stage }) + '\n');
  close(1);
});
