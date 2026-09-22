const { openCalls, closeCalls } = require('./call-inspector.cjs');
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser,page,stage='launch';
const report=value=>process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(()=>{report({stage:'failed',check:stage,reason:'deadline'});close(1);},85000).unref();
const screenshot=async suffix=>{if(page&&process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.${suffix}.png`,fullPage:true});};
(async()=>{
  browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  page=await browser.newPage({viewport:{width:1600,height:1050}});page.setDefaultTimeout(15000);
  let errors=0;page.on('pageerror',()=>errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const input=page.locator('.xterm-helper-textarea');const screen=()=>page.locator('.xterm-rows').innerText();
  let themed=false,trusted=false,ready=false;
  for(let i=0;i<100;i++){
    const value=await screen();
    if(!themed&&/Choose your style|Select a theme/.test(value)){await input.press('Enter');themed=true;}
    else if(!trusted&&/Do you trust|Do you want to work/.test(value)){await input.press('Enter');trusted=true;}
    else if(value.includes('OpenAI Codex')&&value.includes('›')){ready=true;break;}
    await page.waitForTimeout(120);
  }
  assert.ok(ready);const before=await page.evaluate(async()=>(await fetch('/workbench/v1/run')).json());
  const terminal=await input.elementHandle();
  stage='submit';const prompt='R2：验证四种文件修改及失败。';
  await input.evaluate((element,value)=>{const transfer=new DataTransfer();transfer.setData('text/plain',value);element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true}));},prompt);
  await expect.poll(screen).toContain(prompt);await input.press('Enter');
  const messages=page.locator('.wb-message-list');const tools=messages.locator('.wb-tool-card');const first=tools.first();
  stage='intermediate';await expect(tools).toHaveCount(1);await expect(first).toContainText('参数生成中');
  await expect(first).toHaveAttribute('data-category','patch');await expect(first).toHaveAttribute('data-execution','unobserved');
  await expect(first.locator('.wb-diff-file')).toHaveCount(0);
  const card=await first.elementHandle();const parameters=first.locator('.wb-tool-arguments');
  await parameters.locator('summary').press('Enter');const parameterNode=await parameters.elementHandle();
  report({stage:'intermediate',argumentsStreaming:true,noPrematureDiff:true});
  stage='proposal';await expect(first.locator('.wb-diff-file')).toHaveCount(4);
  await expect(first.locator('.wb-tool-result')).toContainText('Success');
  await expect(first).toHaveAttribute('data-execution','succeeded');
  await expect(first.locator('.wb-tool-result')).toContainText('原生运行记录');
  await expect(first.locator('.wb-tool-result [aria-label="stdout"]')).toContainText('Success');
  await expect(first).toContainText('模型提出的修改内容');await expect(first).toContainText('删除行数未知');
  await expect(first).toContainText('move-from.txt → moved.txt');await expect(first).toContainText('+2');
  await expect(parameters).toHaveAttribute('open','');
  assert.equal(await parameterNode.evaluate(element=>element===document.querySelector('.wb-tool-arguments')),true);
  await parameters.locator('summary').press('Enter');
  const files=first.locator('.wb-diff-file');await files.nth(1).locator('summary').press('Enter');
  await files.nth(2).locator('summary').press('Enter');
  await screenshot('diff');
  const focused=files.nth(1).locator('summary');await focused.focus();const focusedNode=await focused.elementHandle();
  report({stage:'proposal',files:4,executionSeparate:true});
  stage='failed-result';await expect(messages).toContainText('R2_PATCH_DONE');await expect(tools).toHaveCount(3);
  await expect(tools.nth(1).locator('.wb-tool-result')).toContainText('missing.txt');
  await expect(tools.nth(1)).toHaveAttribute('data-execution','result_observed');
  await expect(tools.nth(2)).toHaveAttribute('data-execution','failed');
  await expect(tools.nth(2).locator('.wb-tool-result [aria-label="stderr"]')).toContainText('blocker');
  await expect(tools.nth(2).locator('.wb-tool-result details')).toHaveAttribute('open','');
  await expect(files.nth(1)).toHaveAttribute('open','');
  assert.equal(await focusedNode.evaluate(element=>element===document.activeElement),true);
  assert.equal(await card.evaluate(element=>element===document.querySelector('.wb-message-list .wb-tool-card')),true);
  assert.equal(await page.evaluate(()=>Boolean(window.patchInjected)),false);await expect(messages.locator('img,svg,script')).toHaveCount(0);
  await tools.first().getByRole('button',{name:'调用详情',exact:true}).click();
  const native=page.locator('.wb-call-inspector .wb-native-file-changes');
  await native.locator('summary').press('Enter');await expect(native).toContainText('执行成功');await expect(native).toContainText('执行失败');
  await expect(native).toContainText('moved.txt');await closeCalls(page);
  await expect(files.nth(1)).toHaveAttribute('open','');assert.equal(await terminal.evaluate(element=>element===document.querySelector('.xterm-helper-textarea')),true);
  stage='reload';await page.reload();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');await expect(tools).toHaveCount(3);
  await expect(first.locator('.wb-diff-file')).toHaveCount(4);await expect(first).toHaveAttribute('data-execution','succeeded');
  await expect(tools.nth(1)).toHaveAttribute('data-execution','result_observed');await expect(tools.nth(2)).toHaveAttribute('data-execution','failed');
  const after=await page.evaluate(async()=>(await fetch('/workbench/v1/run')).json());assert.equal(after.processId,before.processId);assert.equal(after.runEpoch,before.runEpoch);
  for(const width of [1024,736,320]){await page.setViewportSize({width,height:900});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);}
  await screenshot('narrow');await page.setViewportSize({width:1600,height:1050});
  await expect(messages.locator('[data-role="user"]')).toHaveCount(1);await expect(messages.locator('[data-role="assistant"]')).toHaveCount(4);
  await tools.nth(2).scrollIntoViewIfNeeded();await screenshot('complete');
  assert.equal(errors,0);await input.press('Control+d');await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  report({stage:'complete',toolCards:3,files:4,userMessages:1,modelMessages:4,nativeFinalStates:true,validationFailureUnconfirmed:true,sameCliProcess:true,pageErrors:errors,browser:browser.version()});await close(0);
})().catch(async()=>{await screenshot('failed').catch(()=>{});report({stage:'failed',check:stage,reason:'assertion (private URL/output suppressed)'});close(1);});
