const { readRun, runApiPath } = require('./workbench-api.cjs');
// Native interactions through real xterm/PTY. The route only injects a lost ACK;
// all other WebSocket frames pass unchanged to the production server.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 110000).unref();
const text = () => page.locator('.xterm-rows').innerText();
const input = () => page.locator('.xterm-helper-textarea');
const owned = () => expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
async function paste(value) {
  await input().evaluate((element, value) => {
    const transfer = new DataTransfer(); transfer.setData('text/plain', value);
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  }, value);
}
async function submit(value) { await input().focus(); await paste(value); await expect.poll(text).toContain(value); await input().press('Enter'); }
async function screenshot(suffix) {
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${suffix}.png`, fullPage: true });
}
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  page = await context.newPage(); page.setDefaultTimeout(15000);
  let current, holdNextAck = false, droppedAck = false, terminalInputs = 0, connections = 0, reconnects = 0, pageErrors = 0, faults = 0;
  page.on('pageerror', () => pageErrors++);
  await page.routeWebSocket(url => url.pathname === runApiPath(process.env.WORKBENCH_PROBE_URL, '/terminal'), route => {
    const server = route.connectToServer(); const pair = { route, server, lose: undefined }; current = pair; connections++;
    route.onMessage(message => {
      const frame = JSON.parse(message);
      if (frame.command?.type === 'input') { terminalInputs++; if (holdNextAck) { pair.lose = frame.id; holdNextAck = false; } }
      if (frame.command?.type === 'reconnect') reconnects++;
      server.send(message);
    });
    server.onMessage(message => {
      const frame = JSON.parse(message);
      if (frame.type === 'fault' || frame.fault) faults++;
      if (frame.type === 'ack' && frame.id === pair.lose) {
        droppedAck = true; pair.lose = undefined;
        void Promise.all([server.close(), route.close()]); return;
      }
      route.send(message);
    });
  });
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');
  await owned();
  await expect.poll(() => connections).toBe(1);
  stage = 'keyboard-navigation';
  const toggle = page.getByRole('button',{name:'终端',exact:true});
  const terminalElement = await page.locator('.xterm-helper-textarea').elementHandle();
  await page.keyboard.press('Tab');
  await expect(page.getByRole('link', { name: '← 全部历史', exact: true })).toBeFocused();
  // Follow the shipped header order, including settings and device access.
  for (const name of ['手机接入', '设置', '终端']) {
    stage = `keyboard-navigation-${name}`;
    await page.keyboard.press('Tab');
    await expect(page.getByRole('button', { name, exact: true })).toBeFocused();
  }
  stage = 'keyboard-navigation-terminal';
  assert.equal(await toggle.evaluate(element=>getComputedStyle(element).outlineStyle),'solid');
  await page.keyboard.press('Space'); await expect(page.locator('.wb-terminal-container')).toBeHidden();
  await page.keyboard.press('Enter'); await expect(page.locator('.wb-terminal-container')).toBeVisible();
  assert.equal(await terminalElement.evaluate(element=>element===document.querySelector('.xterm-helper-textarea')),true);
  await page.keyboard.press('Tab'); await expect(page.getByRole('button',{name:'实时对话',exact:true})).toBeFocused();
  await page.keyboard.press('Tab'); await expect(page.getByRole('button',{name:'历史记录',exact:true})).toBeFocused();
  for (const name of ['文件', '搜索代码', 'Git', '用量与状态']) {
    stage = `keyboard-navigation-${name}`;
    await page.keyboard.press('Tab');
    await expect(page.getByRole('button', { name, exact: true })).toBeFocused();
  }
  stage = 'keyboard-navigation-overview';
  await page.keyboard.press('Space'); await expect(page.getByRole('region',{name:'用量与状态'})).toBeVisible();
  stage = 'keyboard-navigation-open-calls';
  await page.getByRole('button',{name:'查看调用记录',exact:true}).press('Enter');
  await expect(page.getByRole('region',{name:'调用记录',exact:true})).toBeVisible();
  stage = 'keyboard-navigation-close-calls';
  await page.keyboard.press('Escape');
  await expect(page.getByRole('button',{name:'用量与状态',exact:true})).toBeFocused();
  await expect(page.locator('.wb-message-list')).toBeVisible();
  stage = 'keyboard-navigation-no-input';
  assert.equal(terminalInputs,0,'page navigation must not send native input');
  report({stage:'keyboard-navigation',terminalPreserved:true,inputs:0});
  const before = await readRun(page, '/run');
  stage = 'native-initialization';
  let modelNoticeHandled = false;
  let themed = false, trusted = false, ready = false;
  for (let n=0;n<100;n++) {
    const screen = await text();
    if (screen.includes('Try new model') && screen.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await input().press('ArrowDown'); await input().press('Enter');
      }
    } else if (!themed && /Choose your style|Select a theme/.test(screen)) { await input().press('Enter'); themed=true; }
    else if (!trusted && /Do you trust|Do you want to work|Trust this folder\?/.test(screen)) { await input().press('Enter'); trusted=true; }
    else if (screen.includes('OpenAI Codex') && screen.includes('›')) {ready=true;break;}
    await page.waitForTimeout(150);
  }
  assert.ok(ready); report({stage:'native-ready'});
  stage = 'native-model-picker';
  await submit('/model'); await expect.poll(text).toContain('Select Model');
  await screenshot('picker'); await input().press('Escape');
  await expect.poll(text).not.toContain('Select Model');
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  report({stage:'picker-cancelled'});

  stage = 'uncertain-input-disconnect';
  await input().focus(); holdNextAck=true; await paste('R1_RECONNECT_DRAFT');
  await expect.poll(() => droppedAck).toBe(true);
  const inputsAtLoss = terminalInputs;
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','false');
  await expect(page.locator('.wb-terminal')).toContainText('不会自动重发');
  await page.keyboard.insertText('MUST_NOT_SEND_OFFLINE');
  await owned(); await expect.poll(text).toContain('R1_RECONNECT_DRAFT');
  assert.equal(terminalInputs, inputsAtLoss, 'offline input and reconnect must not replay bytes');
  assert.equal((await text()).split('R1_RECONNECT_DRAFT').length-1,1);
  stage='uncertainty-after-reconnect';
  await expect(page.locator('.wb-terminal')).toContainText('不会自动重发');
  await page.reload(); await owned();
  await expect(page.locator('.wb-terminal')).toContainText('不会自动重发');
  assert.equal(terminalInputs,inputsAtLoss,'reload with an uncertain input must not resend it');
  await expect.poll(text).toContain('R1_RECONNECT_DRAFT');
  await page.getByRole('button',{name:'已检查终端',exact:true}).click();
  await expect(page.locator('.wb-terminal')).not.toContainText('不会自动重发');
  await input().press('Control+u'); await expect.poll(text).not.toContain('R1_RECONNECT_DRAFT');
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  report({stage:'short-disconnect',lostAckNotReplayed:true,uncertaintyVisible:true});

  stage='native-approval';
  await submit('R1_APPROVAL_REQUEST：执行合成测试的打印命令。');
  await expect.poll(text).toContain('Would you like to run');
  await expect.poll(text).toContain('R1_APPROVED_TOOL_OK');
  await screenshot('approval');
  const beforeApprovalReconnect = terminalInputs;
  await Promise.all([current.server.close(),current.route.close()]);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','false');
  await owned(); await expect.poll(text).toContain('Would you like to run');
  assert.equal(terminalInputs,beforeApprovalReconnect,'reconnect must not approve automatically');
  await input().press('Enter');
  await expect(page.locator('.wb-message-list')).toContainText('R1_APPROVAL_DONE');
  report({stage:'native-approval-answered',pendingSurvivedReconnect:true});

  stage='native-plan-mode';
  await submit('/plan'); await expect.poll(text).toContain('Plan');
  stage='native-question';
  await submit('R1_QUESTION_REQUEST：请让我选择合成数据的排序方式。');
  await expect.poll(text).toContain('请选择本次合成测试的排序方式');
  await expect.poll(text).toContain('Price'); await screenshot('question');
  const beforeRefresh=terminalInputs;
  await page.reload(); await owned();
  await expect.poll(text).toContain('请选择本次合成测试的排序方式');
  assert.equal(terminalInputs,beforeRefresh,'refresh must not submit the question answer');
  await input().press('ArrowDown'); await input().press('Enter');
  await expect(page.locator('.wb-message-list')).toContainText('R1_QUESTION_DONE');
  const after = await readRun(page, '/run');
  assert.equal(after.processId,before.processId); assert.equal(after.runEpoch,before.runEpoch);
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(2);
  await screenshot('complete');
  stage='stop-run-controls';
  await page.getByRole('button',{name:'用量与状态',exact:true}).press('Enter');
  const details=page.getByRole('region',{name:'用量与状态'});
  await details.getByRole('button',{name:'停止当前运行',exact:true}).press('Enter');
  const confirmation=page.getByRole('alertdialog',{name:'确认停止运行'});
  await confirmation.getByRole('button',{name:'取消',exact:true}).press('Space');
  await expect(confirmation).toHaveCount(0); await owned();
  await details.getByRole('button',{name:'停止当前运行',exact:true}).press('Enter');
  await confirmation.getByRole('button',{name:'确认停止',exact:true}).press('Enter');
  await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  await expect(page.locator('.wb-message-list')).toContainText('R1_QUESTION_DONE');
  const stopped=await readRun(page, '/run');
  assert.equal(stopped.processId,before.processId); assert.equal(stopped.runEpoch,before.runEpoch);
  assert.equal(pageErrors,0); assert.equal(faults,0);
  report({stage:'complete',browser:browser.version(),connections,reconnects,terminalInputs,pageErrors,faults,sameCliProcess:true,keyboardNavigation:true,stopCancelledThenConfirmed:true,stoppedReadingAvailable:true});
  await close(0);
})().catch(async () => { await screenshot('failed').catch(()=>{}); report({stage:'failed',check:stage,reason:'assertion (private URL/output suppressed)'}); close(1); });
