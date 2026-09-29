const { readRun } = require('./workbench-api.cjs');
const { openCalls, closeCalls } = require('./call-inspector.cjs');
// Only the installed Chrome and CLI against a local synthetic provider.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage='launch';
const old=process.env.WORKBENCH_PROBE_OLD_EPOCH;
const report=value=>process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(()=>{report({stage:'failed',check:stage,reason:'deadline'});close(1);},60000).unref();
const screenshot=async suffix=>{if(page&&process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.${suffix}.png`,fullPage:true});};
(async()=>{
  browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  page=await browser.newPage({viewport:{width:1600,height:1000}});page.setDefaultTimeout(12000);
  let errors=0;page.on('pageerror',()=>errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const input=page.locator('.xterm-helper-textarea'),screen=()=>page.locator('.xterm-rows').innerText();
  const terminal=await input.elementHandle();const run=await readRun(page, '/run');
  stage='native-ready';let modelNoticeHandled = false;
  let themed=false,trusted=false,ready=false;
  for(let n=0;n<100;n++){
    const text=await screen();
    if (text.includes('Try new model') && text.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await input.press('ArrowDown'); await input.press('Enter');
      }
    } else if(!themed&&/Choose your style|Select a theme/.test(text)){await input.press('Enter');themed=true;}
    else if(!trusted&&/Do you trust|Do you want to work|Trust this folder\?/.test(text)){await input.press('Enter');trusted=true;}
    else if(text.includes('OpenAI Codex')&&text.includes('›')){ready=true;break;}
    await page.waitForTimeout(100);
  }
  assert.ok(ready);await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  if(old){
    stage='saved-history';assert.notEqual(old,run.runEpoch);
    await page.getByRole('button',{name:'历史记录',exact:true}).focus();await page.keyboard.press('Enter');
    await page.locator('.wb-history-list button').filter({hasText:old.slice(0,8)}).click();
    const history=page.locator('.wb-saved-reading');
    await expect(history).toContainText('R3_SAVED_REPLY');await expect(history).toContainText('正常结束');
    await expect(history.locator('[data-role="user"]')).toHaveCount(1);
    await expect(history.locator('.wb-tool-card:visible')).toHaveAttribute('data-execution','succeeded');
    await expect(history.locator('.wb-tool-result:visible')).toContainText('R3_NATIVE_TOOL');
    await expect(history).toContainText('[已脱敏]');await expect(history.locator('svg,script,img')).toHaveCount(0);
    await expect(page.getByRole('button',{name:'查看更早的记录',exact:true})).toHaveCount(0);await screenshot('conversation');
    await history.getByRole('button',{name:'调用详情',exact:true}).first().click();
    const context=page.locator('.wb-call-inspector .wb-request-context');
    await expect(context.locator('.wb-context-entry').first()).toBeVisible();
    assert.equal(await page.evaluate(()=>Boolean(window.r3Injected)),false);
    await screenshot('history');
    for(const width of [1024,736,320]){await page.setViewportSize({width,height:900});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);}
    await screenshot('narrow');await page.setViewportSize({width:1600,height:1000});
    await closeCalls(page);
    const before=await readRun(page, '/live/snapshot');assert.equal(before.requests.length,0);
    assert.equal(await terminal.evaluate(node=>node===document.querySelector('.xterm-helper-textarea')),true);
  }
  stage='native-submit';const prompt=old?'R3_CURRENT：这是新运行，请返回当前运行标记。':'R3_SEED：执行一次合成命令并保存回复。';
  await input.evaluate((element,value)=>{const data=new DataTransfer();data.setData('text/plain',value);element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true}));},prompt);
  await expect.poll(screen).toContain(prompt);await input.press('Enter');
  const expected=old?'R3_CURRENT_REPLY':'R3_SAVED_REPLY';await expect.poll(screen).toContain(expected);
  if(old){
    await expect(page.locator('.wb-saved-reading')).not.toContainText('R3_CURRENT_REPLY');
    await page.getByRole('button',{name:'返回实时阅读',exact:true}).click();await expect(page.locator('.wb-message-list')).toBeVisible();
  }
  const messages=page.locator('.wb-message-list');await expect(messages).toContainText(expected);await expect(messages.locator('[data-role="user"]')).toHaveCount(1);
  if(!old){await expect(messages.locator('.wb-tool-card')).toHaveAttribute('data-execution','succeeded');}
  stage='saved-watermark';await expect.poll(async()=>readRun(page, '/live/snapshot').then(s => s.recorder === 'saved' && s.persistedThroughViewSeq === s.viewSeq)).toBe(true);
  if(old){
    stage='reading-anchor';await messages.evaluate(node=>{node.scrollTop=180;});const scroll=await messages.evaluate(node=>node.scrollTop);
    await page.getByRole('button',{name:'历史记录',exact:true}).click();await page.getByRole('button',{name:'返回实时阅读',exact:true}).click();
    assert.ok(Math.abs((await messages.evaluate(node=>node.scrollTop))-scroll)<3);
    assert.equal(await terminal.evaluate(node=>node===document.querySelector('.xterm-helper-textarea')),true);
    await page.reload();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
    const after=await readRun(page, '/run');assert.equal(after.processId,run.processId);assert.equal(after.runEpoch,run.runEpoch);
  }
  await screenshot(old?'current':'seed');await input.press('Control+d');await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  assert.equal(errors,0);report({stage:'complete',phase:old?'restart':'seed',restoredNativeTool:Boolean(old),sameTerminal:true,noAutomaticResume:true,pageErrors:errors,browser:browser.version()});await close(0);
})().catch(async()=>{await screenshot('failed').catch(()=>{});report({stage:'failed',check:stage,reason:'assertion (private output suppressed)'});close(1);});
