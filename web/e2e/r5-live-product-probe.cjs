const { readRun } = require('./workbench-api.cjs');
// Explicit real-provider probe: synthetic project, native input, no route mocks.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const started = Date.now();
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 970000).unref();
setInterval(() => report({ stage: 'progress', check: stage, elapsedMs: Date.now() - started }), 15000).unref();
const text = () => page.locator('.xterm-rows').innerText();
const input = () => page.locator('.xterm-helper-textarea');
const snapshot = () => readRun(page, '/live/snapshot');
async function paste(value) {
  await input().focus();
  await input().evaluate((element, value) => {
    const transfer = new DataTransfer(); transfer.setData('text/plain', value);
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  }, value);
}
async function submit(value) { await paste(value); await page.waitForTimeout(250); await input().press('Enter'); }
async function response(marker, receiving) {
  const card = page.locator('.wb-message-list .wb-model-message').filter({ hasText: marker }).last();
  await expect(card).toBeVisible({ timeout: 280000 });
  if (receiving) await expect(card.locator('.wb-message-status')).toContainText('正在接收');
  return card;
}
async function finished(card) {
  await expect(card.locator('.wb-message-status')).toContainText('模型响应已结束', { timeout: 280000 });
}
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  page = await context.newPage(); page.setDefaultTimeout(20000);
  let errors = 0, faults = 0, nonFocusInputs = 0;
  page.on('pageerror', () => errors++);
  page.on('websocket', socket => {
    socket.on('framereceived', ({ payload }) => { try { const f = JSON.parse(payload); if (f.type === 'fault' || f.fault) faults++; } catch {} });
    socket.on('framesent', ({ payload }) => { try { const f = JSON.parse(payload); if (f.command?.type === 'input') {
      const bytes = Buffer.from(f.command.data, 'base64').toString('utf8');
      if (!['\x1b[I', '\x1b[O'].includes(bytes)) nonFocusInputs++;
    } } catch {} });
  });
  stage = 'pair-page';
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  stage = 'terminal-auto-input';
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  const before = await readRun(page, '/run');
  stage = 'native-initialization';
  let modelNoticeHandled = false;
  let themed = false, trusted = false, ready = false;
  for (let n = 0; n < 120; n++) {
    const screen = await text();
    if (screen.includes('Try new model') && screen.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await input().press('ArrowDown'); await input().press('Enter');
      }
    } else if (!themed && /Choose your style|Select a theme/.test(screen)) { await input().press('Enter'); themed = true; }
    else if (!trusted && /Do you trust|Do you want to work|Trust this folder\?/.test(screen)) { await input().press('Enter'); trusted = true; }
    else if (screen.includes('OpenAI Codex') && screen.includes('›')) { ready = true; break; }
    await page.waitForTimeout(150);
  }
  assert.ok(ready);
  stage = 'native-slash-picker';
  await submit('/model'); await expect.poll(text).toContain('Select Model');
  await input().press('Escape'); await expect.poll(text).not.toContain('Select Model');
  await expect(page.locator('.wb-user-body')).toHaveCount(0);
  report({ stage: 'native-ready', slashPickerCancelled: true });

  stage = 'real-tool-and-stream';
  const prompt = '这是隔离验收。只用终端工具读取当前目录的 R5-check.txt，不访问其他文件或网络。\n读取成功后，回复以 R5_LIVE_TEXT_7391 开头，先复述文件内容，再写一篇约 800 字的中文月球图书馆故事；直接输出正文，不要调用更多工具。';
  await paste(prompt); await page.waitForTimeout(250);
  await expect(page.locator('.wb-user-body')).toHaveCount(0);
  const submittedAt = Date.now(); await input().press('Enter');
  await expect(page.locator('.wb-user-body').first()).toHaveText(prompt);
  const story = await response('R5_LIVE_TEXT_7391', true);
  report({ stage: 'real-midstream', submitToVisibleMs: Date.now() - submittedAt });
  await finished(story);
  const first = await snapshot();
  assert.ok(first.nativeCommands.some(c => c.status === 'completed' && c.exitCode === 0 && c.output.includes('R5_TOOL_FILE_7391')));
  await expect(page.locator('.wb-tool-card').first()).toBeVisible();
  assert.ok((await story.innerText()).includes('R5_TOOL_FILE_7391'));
  report({ stage: 'real-tool-verified', nativeExitCode: 0, separateToolCard: true, userMultilineExact: true, submitToResponseEndMs: Date.now() - submittedAt });

  stage = 'refresh-with-draft';
  await paste('R5_UNSENT_DRAFT_7391');
  await expect.poll(text).toContain('R5_UNSENT_DRAFT_7391');
  const inputsBeforeRefresh = nonFocusInputs;
  await page.reload(); await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect.poll(text).toContain('R5_UNSENT_DRAFT_7391');
  assert.equal(nonFocusInputs, inputsBeforeRefresh);
  const after = await readRun(page, '/run');
  assert.equal(after.processId, before.processId); assert.equal(after.runEpoch, before.runEpoch);
  await expect(page.locator('.wb-user-body')).toHaveCount(1);
  await input().press('Control+u');
  report({ stage: 'refresh-verified', sameCli: true, draftPreserved: true, noReplay: true });

  stage = 'real-stream-cancel';
  await submit('不调用工具。回复以 R5_CANCEL_7391 开头，然后从 1 到 500 逐行输出，每行一句完整的中文月球图书馆故事。不要简写。');
  const cancelled = await response('R5_CANCEL_7391', true);
  await input().press('Escape');
  await expect(cancelled.locator('.wb-message-status')).not.toContainText('正在接收', { timeout: 30000 });
  report({ stage: 'cancel-verified', interruptedWhileReceiving: true });

  stage = 'native-image-attachment';
  await page.waitForTimeout(500);
  const priorUsers = await page.locator('.wb-user-body').count();
  await paste(process.env.WORKBENCH_PROBE_IMAGE);
  await expect.poll(text).toContain('[Image #1]');
  await expect(page.locator('.wb-user-body')).toHaveCount(priorUsers);
  report({ stage: 'image-attached', nativePlaceholder: true, noAutomaticEnter: true });
  await submit('仅观察这张附图，不调用工具。回复以 R5_IMAGE_7391 开头，描述图片左半边和右半边的颜色，然后用约 200 字说明这种配色。');
  const image = await response('R5_IMAGE_7391', false); await finished(image);
  const imageText = await image.innerText(); assert.match(imageText, /红/); assert.match(imageText, /蓝/);
  const final = await snapshot();
  assert.ok(final.usageSummary);
  report({ stage: 'real-image-verified', redAndBlueRecognized: true, sameCliRecovery: true, requestCount: final.requests.length,
    diagnosticCodes: [...new Set(final.diagnostics.map(d => d.code))], capture: final.capture });
  if (process.env.WORKBENCH_PROBE_CONTROLS === '1') {
    stage = 'real-native-approval';
    await submit('这是原生审批验收。请仅执行 printf 命令打印 R5_APPROVED_7391，不访问文件或网络。调用 exec_command 时显式设置 sandbox_permissions 为 require_escalated，justification 为 Print the synthetic R5 approval marker only，以便我在原生终端审批。不要改用其他方式绕过审批。执行成功后仅回复 R5_APPROVAL_DONE。');
    await expect.poll(text, { timeout: 280000 }).toMatch(/Would you like to run|Do you want to run|Approve/);
    await expect.poll(text).toContain('R5_APPROVED_7391');
    const priorApprovalInputs = nonFocusInputs;
    await page.reload(); await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
    await expect.poll(text).toMatch(/Would you like to run|Do you want to run|Approve/);
    assert.equal(nonFocusInputs, priorApprovalInputs);
    await input().press('Enter');
    const approved = await response('R5_APPROVAL_DONE', false); await finished(approved);
    report({ stage: 'real-approval-verified', refreshDidNotAnswer: true, nativeAnswer: true });
    stage = 'real-native-question';
    await submit('/plan'); await expect.poll(text).toContain('Plan');
    await submit('这是隔离追问验收。请使用 request_user_input 工具问我“R5请选择颜色”，提供 Blue 与 Red 两项。请必须先等我的选择，不要自行假设或写方案。收到选择后只回复 R5_QUESTION_DONE 和我选择的英文颜色，不调用其他工具。');
    await expect.poll(text, { timeout: 280000 }).toContain('Question 1/1');
    await expect.poll(text, { timeout: 280000 }).toContain('R5请选择颜色');
    await expect.poll(text).toContain('Blue'); await expect.poll(text).toContain('Red');
    const priorQuestionInputs = nonFocusInputs;
    await page.reload(); await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
    await expect.poll(text).toContain('R5请选择颜色');
    await expect.poll(text).toMatch(/1\. Blue/);
    assert.equal(nonFocusInputs, priorQuestionInputs);
    await input().press('Enter');
    const answered = await response('R5_QUESTION_DONE', false); await finished(answered);
    assert.match(await answered.innerText(), /Blue/);
    report({ stage: 'real-question-verified', refreshDidNotAnswer: true, selectedBlue: true });
  }
  stage = 'native-exit';
  await input().press('Control+d');
  await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  assert.equal(errors, 0); assert.equal(faults, 0);
  report({ stage: 'complete', browser: browser.version(), pageErrors: errors, terminalFaults: faults, elapsedMs: Date.now() - started });
  await close(0);
})().catch(async () => {
  const metrics = await page?.evaluate(() => ({ ready: document.querySelector('.wb-terminal')?.getAttribute('data-ready'),
    owned: document.querySelector('.wb-terminal')?.getAttribute('data-owned'), terminalExists: !!document.querySelector('.wb-xterm'),
    terminalEnded: document.querySelector('.wb-terminal-status')?.textContent === '已结束',
    documentReady: document.readyState })).catch(() => undefined);
  if (metrics?.terminalEnded && stage === 'terminal-auto-input') report({ stage: 'native-startup-diagnostic', text: await text() });
  report({ stage: 'failed', check: stage, reason: 'assertion; private output suppressed', metrics }); close(1);
});
