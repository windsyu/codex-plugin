const { readRun } = require('./workbench-api.cjs');
const { openCalls, closeCalls } = require('./call-inspector.cjs');
// Real installed Chrome + ordinary CLI, all state/model data is synthetic.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, diagnosticPage, diagnosticFaults = 0, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);
const pngPath = prefix => prefix.endsWith('.png') ? prefix : `${prefix}.png`;

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 85000).unref();
const text = page => page.locator('.xterm-rows').innerText();
const owned = page => expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
const prompt = '请回复 R1_NATIVE_BROWSER_OK，不运行工具。\n第二行：保留换行与 <b>原样文本</b>。';
const delayedUser = process.env.WORKBENCH_PROBE_LATE_USER !== 'false';
const unknownRequest = process.env.WORKBENCH_PROBE_UNKNOWN !== 'false';
async function paste(page, value) {
  await page.locator('.xterm-helper-textarea').evaluate((element, value) => {
    const transfer = new DataTransfer(); transfer.setData('text/plain', value);
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  }, value);
}
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage(); diagnosticPage = page; page.setDefaultTimeout(15000);
  let errors = 0, terminalFaults = 0, inputFrames = 0, resizeFrames = 0, reconnectFrames = 0;
  const inputKinds = [];
  const watchFaults = socket => socket.on('framereceived', ({ payload }) => {
    try { const frame = JSON.parse(payload); if (frame.type === 'fault' || frame.fault) { terminalFaults++; diagnosticFaults = terminalFaults; } } catch {}
  });
  page.on('pageerror', () => errors++);
  page.on('websocket', watchFaults);
  page.on('websocket', socket => socket.on('framesent', ({ payload }) => {
    try { const frame = JSON.parse(payload); if (frame.command?.type === 'input') { inputFrames++; const value = Buffer.from(frame.command.data, 'base64').toString('utf8'); inputKinds.push(value === '\x1b[I' || value === '\x1b[O' ? 'focus' : 'other'); } if (frame.command?.type === 'resize') resizeFrames++; if (frame.command?.type === 'reconnect') reconnectFrames++; } catch {}
  }));
  stage = 'pair-and-auto-input';
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  await owned(page);
  await expect(page.getByRole('button', { name: /启用输入|释放输入权|在此输入/ })).toHaveCount(0);
  assert.equal(new URL(page.url()).hash, '');
  const before = await readRun(page, '/run');
  stage = 'native-initialization';
  let modelNoticeHandled = false;
  let themed = false, trusted = false, ready = false;
  for (let n = 0; n < 80; n++) {
    const screen = await text(page);
    if (screen.includes('Try new model') && screen.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await page.locator('.xterm-helper-textarea').press('ArrowDown'); await page.locator('.xterm-helper-textarea').press('Enter');
      }
    } else if (!themed && (screen.includes('Choose your style') || screen.includes('Select a theme'))) {
      await page.locator('.xterm-helper-textarea').press('Enter'); themed = true;
    } else if (!trusted && (screen.includes('Do you trust') || screen.includes('Do you want to work') || screen.includes('Trust this folder?'))) {
      await page.locator('.xterm-helper-textarea').press('Enter'); trusted = true;
    } else if (screen.includes('OpenAI Codex') && screen.includes('›')) { ready = true; break; }
    await page.waitForTimeout(150);
  }
  assert.ok(ready, 'native prompt should be visible through xterm');
  report({ stage: 'native-ready', inputFrames });
  if (process.env.WORKBENCH_PROBE_WORKSPACE === 'true') {
    stage = 'workspace-with-native-cli';
    const native = await page.locator('.xterm-helper-textarea').elementHandle(); const priorInput = inputFrames;
    stage = 'workspace-root-with-native-cli'; assert.ok(before.workspaceRoot);
    stage = 'workspace-open-with-native-cli'; await page.getByRole('button', { name: '文件', exact: true }).click();
    await page.getByRole('treeitem').filter({ hasText: 'R4-check.txt' }).click();
    await expect(page.getByRole('region', { name: '代码内容', exact: true })).toContainText('native workspace probe');
    stage = 'workspace-return-with-native-cli'; await page.getByRole('button', { name: '关闭文件阅读' }).click();
    await page.getByRole('button', { name: '收起项目面板' }).click();
    const current = await readRun(page, '/run');
    stage = 'workspace-process-with-native-cli'; assert.equal(current.processId, before.processId); assert.equal(current.runEpoch, before.runEpoch);
    assert.ok(await native.evaluate(e => e.isConnected)); stage = 'workspace-input-with-native-cli'; assert.ok(inputKinds.slice(priorInput).every(kind => kind === 'focus'), 'navigation may report focus but must not send conversation text or Enter');
    await page.locator('.xterm-helper-textarea').focus();
    report({ stage: 'workspace-native-verified', sameProcess: true, noConversationInput: true, nativeFocusReports: inputKinds.slice(priorInput).length });
  }
  stage = 'native-model-stream';
  await paste(page, prompt);
  await page.waitForTimeout(150);
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  await page.locator('.xterm-helper-textarea').press('Enter');
  await expect(page.locator('.wb-message-list .wb-message-body').filter({ hasText: 'R1_NATIVE_BROWSER_OK' })).toBeVisible();
  await expect(page.locator('.wb-message-list .wb-model-message').filter({ hasText: 'R1_NATIVE_BROWSER_OK' }).locator('.wb-message-status')).toContainText('正在接收');
  await expect(page.locator('.wb-message-list .wb-model-message .wb-message-meta')).toContainText('gpt-6-astra · 响应报告');
  report({ stage: 'intermediate', inputFrames });
  const list = page.locator('.wb-message-list');
  if (!delayedUser) await expect(list.locator('.wb-user-body')).toHaveText(prompt);
  stage = 'wait-scrollable';
  await expect.poll(async () => list.evaluate(element => element.scrollHeight > element.clientHeight + 80)).toBe(true);
  stage = 'pause-follow';
  await list.evaluate(element => { element.scrollTop = 0; });
  await expect(page.getByRole('button', { name: '跟随最新', exact: false })).toBeVisible();
  stage = 'late-native-user-evidence';
  if (delayedUser) await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  const modelCard = page.locator('.wb-message-list .wb-model-message');
  const anchorY = (await modelCard.boundingBox()).y;
  await expect(page.locator('.wb-message-list .wb-user-body')).toHaveText(prompt);
  assert.equal(await page.locator('.wb-user-body b').count(), 0, 'user content is literal text');
  assert.ok(Math.abs((await modelCard.boundingBox()).y - anchorY) <= 3, 'late native user must preserve the visible model anchor');
  await expect(page.locator('.xterm-helper-textarea')).toBeFocused();
  const roles = await list.locator('[data-role]').evaluateAll(elements => elements.map(element => element.dataset.role));
  assert.deepEqual(roles, ['user', 'assistant']);
  stage = 'finish-while-paused';
  await expect(page.locator('.wb-message-list .wb-message-body').filter({ hasText: 'R1_STREAM_DONE' })).toBeVisible();
  assert.ok(Math.abs((await modelCard.boundingBox()).y - anchorY) <= 3, 'streaming should retain the reading anchor');
  await page.getByRole('button', { name: '跟随最新', exact: false }).click();
  stage = 'request-purpose';
  await expect(page.locator('.wb-message-list .wb-model-message')).toHaveCount(1);
  await openCalls(page);
  const auxiliary = page.locator('.wb-call-row[data-purpose="auxiliary"]');
  await expect(auxiliary).toHaveCount(1); await auxiliary.click();
  await page.locator('.wb-call-unassigned > summary').click();
  await expect(page.locator('.wb-call-inspector .wb-message-body')).toContainText('本机工作台调试');
  await page.getByRole('button', { name: '全部调用', exact: false }).click();
  const unclassified = page.locator('.wb-call-row[data-purpose="unknown"]');
  await expect(unclassified).toHaveCount(unknownRequest ? 1 : 0);
  if (unknownRequest) {
    await unclassified.click(); await page.locator('.wb-call-unassigned > summary').click();
    await expect(page.locator('.wb-call-inspector .wb-message-body')).toContainText('R1_UNKNOWN_REQUEST_CONTENT');
  }
  await closeCalls(page);
  await expect(page.locator('.wb-message-list')).not.toContainText('R1_UNKNOWN_REQUEST_CONTENT');
  report({ stage: 'request-purpose-verified', conversation: 1, auxiliary: 1, unclassified: unknownRequest ? 1 : 0 });
  stage = 'same-text-new-submission';
  await paste(page, prompt);
  await page.waitForTimeout(150);
  await expect(list.locator('[data-role="user"]')).toHaveCount(1);
  await page.locator('.xterm-helper-textarea').press('Enter');
  await expect(list.locator('[data-role="user"]')).toHaveCount(2);
  await expect(list.locator('.wb-model-message')).toHaveCount(2);
  await expect(list.locator('.wb-model-message').nth(1).locator('.wb-message-body')).toContainText('R1_STREAM_DONE');
  assert.deepEqual(await list.locator('[data-role]').evaluateAll(elements => elements.map(element => element.dataset.role)), ['user', 'assistant', 'user', 'assistant']);
  assert.deepEqual(await list.locator('.wb-user-body').allTextContents(), [prompt, prompt]);
  const messageKeys = await list.locator('[data-reading-key]').evaluateAll(elements => elements.map(element => element.dataset.readingKey));
  report({ stage: 'native-chat-verified', userMessages: 2, modelMessages: 2, repeatedInputKeptDistinct: true, delayedUserEvidence: delayedUser, anchorRetained: true });
  stage = 'panels-and-draft';
  await page.locator('.xterm-helper-textarea').focus();
  await page.keyboard.insertText('保留草稿');
  await expect.poll(() => text(page)).toContain('保留草稿');
  await expect(list.locator('[data-role="user"]')).toHaveCount(2);
  await page.locator('.wb-xterm').evaluate(element => { element.dataset.identityProbe = 'same-node'; });
  await openCalls(page);
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await expect(page.locator('.wb-xterm')).toHaveAttribute('data-identity-probe', 'same-node');
  await owned(page);
  await expect.poll(() => text(page)).toContain('保留草稿');
  await closeCalls(page);
  const beforeReloadInputs = inputFrames;
  report({ stage: 'before-refresh', reconnectFrames, ...(await page.evaluate(() => ({ owned: document.querySelector('.wb-terminal')?.getAttribute('data-owned'), reconnectStored: Object.keys(sessionStorage).filter(key => key.startsWith('workbench-reconnect:')).length, navigation: performance.getEntriesByType('navigation')[0]?.type }))) });
  stage = 'refresh-input-ownership';
  await page.reload(); await owned(page);
  stage = 'refresh-draft';
  await expect.poll(() => text(page)).toContain('保留草稿');
  stage = 'refresh-no-replay';
  assert.equal(inputFrames, beforeReloadInputs, 'refresh must not replay native input');
  const after = await readRun(page, '/run');
  assert.equal(after.processId, before.processId); assert.equal(after.runEpoch, before.runEpoch);
  await expect(list.locator('[data-role="user"]')).toHaveCount(2);
  assert.deepEqual(await list.locator('[data-reading-key]').evaluateAll(elements => elements.map(element => element.dataset.readingKey)), messageKeys);
  report({ stage: 'refresh-kept-process', inputFrames });
  stage = 'second-page-takeover';
  const second = await context.newPage(); diagnosticPage = second; second.setDefaultTimeout(15000);
  second.on('pageerror', () => errors++);
  second.on('websocket', watchFaults);
  await second.goto(page.url());
  await expect(second.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  await expect(second.locator('.wb-terminal')).toHaveAttribute('data-owned', 'false');
  await expect(second.locator('.wb-terminal')).toContainText('另一页面正在使用此终端');
  await owned(page);
  await second.getByRole('button', { name: '在此输入', exact: true }).click(); await owned(second);
  await expect(second.getByRole('button', { name: /启用输入|释放输入权|在此输入|确认接管/ })).toHaveCount(0);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'false');
  const deniedBefore = inputFrames;
  await page.locator('.xterm-helper-textarea').focus(); await page.keyboard.insertText('DENIED');
  await page.waitForTimeout(150); assert.equal(inputFrames, deniedBefore);
  stage = 'responsive';
  for (const width of [1024, 736, 320]) {
    stage = `responsive-overflow-${width}`;
    await second.setViewportSize({ width, height: 800 });
    await second.waitForTimeout(150);
    assert.ok(await second.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), `viewport ${width} must not overflow`);
  }
  await second.setViewportSize({ width: 1440, height: 900 });
  await second.waitForTimeout(300);
  stage = 'responsive-restored-ownership';
  await owned(second);
  stage = 'responsive-restored-draft';
  await expect.poll(() => text(second)).toContain('保留草稿');
  stage = 'responsive-terminal-faults';
  assert.equal(terminalFaults, 0, 'normal viewport resizing must not degrade VT reconstruction');
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) {
    stage = 'responsive-screenshot';
    await second.locator('.wb-message-list').evaluate(element => { element.scrollTop = 0; });
    await second.screenshot({ path: pngPath(process.env.WORKBENCH_PROBE_SCREENSHOT), fullPage: true });
  }
  stage = 'native-exit';
  await second.locator('.xterm-helper-textarea').press('Control+u'); await second.waitForTimeout(200);
  await expect(second.locator('.wb-message-list [data-role="user"]')).toHaveCount(2);
  await second.locator('.xterm-helper-textarea').press('Control+d');
  await expect(second.locator('.wb-terminal-status')).toHaveText('已结束');
  await expect(second.locator('.wb-message-list [data-role="user"]')).toHaveCount(2);
  assert.equal(errors, 0);
  report({ stage: 'complete', browser: browser.version(), inputFrames, resizeFrames, pageErrors: errors, terminalFaults, sameCliProcess: true, automaticInput: true, singleClickPageSwitch: true });
  await close(0);
})().catch(async () => {
  const page = diagnosticPage || browser?.contexts()[0]?.pages()[0];
  const metrics = await page?.evaluate(() => {
    const list = document.querySelector('.wb-message-list');
    return { viewport: { width: innerWidth, height: innerHeight }, documentWidth: document.documentElement.scrollWidth, terminalWidth: document.querySelector('.wb-terminal')?.getBoundingClientRect().width, xtermWidth: document.querySelector('.xterm-screen')?.getBoundingClientRect().width, top: list?.scrollTop, height: list?.scrollHeight, clientHeight: list?.clientHeight, followButton: !!document.querySelector('.wb-follow'), terminalReady: document.querySelector('.wb-terminal')?.getAttribute('data-ready'), owned: document.querySelector('.wb-terminal')?.getAttribute('data-owned'), reconnectStored: Object.keys(sessionStorage).filter(key => key.startsWith('workbench-reconnect:')).length, navigation: performance.getEntriesByType('navigation')[0]?.type };
  }).catch(() => undefined);
  if (page && process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.failed.png`, fullPage: true }).catch(() => {});
  report({ stage: 'failed', check: stage, reason: 'assertion (private URLs/output suppressed)', terminalFaults: diagnosticFaults, metrics });
  close(1);
});
