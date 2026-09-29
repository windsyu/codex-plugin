const { readRun } = require('./workbench-api.cjs');
const { openCalls, closeCalls } = require('./call-inspector.cjs');
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage='launch';
const code=process.env.WORKBENCH_PROBE_CODE==='true';
const report=value=>process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(()=>{report({stage:'failed',check:stage,reason:'deadline'});close(1);},85000).unref();
const screenshot=async suffix=>{if(page&&process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.${code?'code':'command'}.${suffix}.png`,fullPage:true});};
(async()=>{
  browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  page=await browser.newPage({viewport:{width:1600,height:1000}});page.setDefaultTimeout(15000);
  let errors=0,sawTrueColor=false;page.on('pageerror',()=>errors++);
  page.on('websocket',socket=>socket.on('framereceived',({payload})=>{
    try {const frame=JSON.parse(payload);if(frame.type==='output')sawTrueColor ||= /\x1b\[(?:38|48);2;\d+;\d+;\d+m/.test(Buffer.from(frame.data,'base64').toString('utf8'));}catch{}
  }));
  await page.goto(process.env.WORKBENCH_PROBE_URL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');
  stage='static-native-welcome';
  const staticHeading = /Welcome to Codex|Folder access/;
  await expect.poll(()=>page.locator('.xterm-rows').innerText()).toMatch(staticHeading);
  const welcomeLines=(await page.locator('.xterm-rows').innerText()).split('\n');
  assert.ok(welcomeLines.findIndex(line=>staticHeading.test(line))<10,'native onboarding should not be displaced by the ASCII animation');
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const input=page.locator('.xterm-helper-textarea');const screen=()=>page.locator('.xterm-rows').innerText();
  let modelNoticeHandled = false;
  let themed=false,trusted=false,ready=false;
  for(let i=0;i<100;i++){
    const value=await screen();
    if (value.includes('Try new model') && value.includes('Use existing model')) {
      if (!modelNoticeHandled) {
        modelNoticeHandled = true;
        await input.press('ArrowDown'); await input.press('Enter');
      }
    } else if(!themed&&/Choose your style|Select a theme/.test(value)){await input.press('Enter');themed=true;}
    else if(!trusted&&/Do you trust|Do you want to work|Trust this folder\?/.test(value)){await input.press('Enter');trusted=true;}
    else if(value.includes('OpenAI Codex')&&value.includes('›')){ready=true;break;}
    await page.waitForTimeout(120);
  }
  assert.ok(ready);const before=await readRun(page, '/run');
  const terminal=await input.elementHandle();
  const prompt='R2：读取 input.txt、写入 result.txt，并观察一次非零退出码。';
  await input.evaluate((element,value)=>{const transfer=new DataTransfer();transfer.setData('text/plain',value);element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true}));},prompt);
  await expect.poll(screen).toContain(prompt);await input.press('Enter');
  const messages=page.locator('.wb-message-list');const tools=messages.locator('.wb-tool-card');
  stage='intermediate';await expect(tools).toHaveCount(1);await expect(tools.first()).toContainText('参数生成中');
  await expect(tools.first()).toContainText('尚未观察到执行');await expect(tools.first()).toHaveAttribute('data-category',code?'code':'command');
  await expect(messages.locator('[data-role="assistant"]')).toContainText('这只是模型文字中的代码示例');
  await expect(messages.locator('[data-role="user"]')).toHaveCount(1);
  await expect(messages.locator('[data-role="user"]')).toContainText(prompt);
  const firstTool=await tools.first().elementHandle();
  await screenshot('partial');report({stage:'intermediate',codeMode:code,argumentsStreaming:true,executionUnobserved:true});
  stage='results';await expect(messages).toContainText('R2_TOOLS_DONE');await expect(tools).toHaveCount(3);
  await expect(messages.locator('[data-role="user"]')).toHaveCount(1);await expect(messages.locator('[data-role="assistant"]')).toHaveCount(4);
  assert.equal(await firstTool.evaluate(element=>element===document.querySelector('.wb-message-list .wb-tool-card')),true);
  for(let index=0;index<3;index++){
    const tool=tools.nth(index);await expect(tool).toContainText('参数已生成');
    await expect(tool).toHaveAttribute('data-execution',code?'result_observed':index===2?'failed':'succeeded');
    await expect(tool.locator('.wb-tool-result')).toContainText(['R2_READ_OK','R2_EDIT_OK','R2_FAIL_OK'][index]);
  }
  assert.equal(await page.evaluate(()=>Boolean(window.r2Injected)),false);await expect(messages.locator('svg,script,img')).toHaveCount(0);
  await expect(tools.locator('.wb-command-preview')).toHaveCount(code?0:3);
  if(!code){await expect(tools.nth(2)).toContainText('退出码：7');await expect(tools.nth(2).locator('.wb-tool-result details')).toHaveAttribute('open','');}
  const parameters=tools.first().locator('details').first();await parameters.locator('summary').press('Enter');await expect(parameters).toHaveAttribute('open','');
  await tools.first().getByRole('button',{name:'调用详情',exact:true}).click();await expect(page.locator('.wb-call-inspector')).toContainText(code?'additional_tools':'tools');
  stage='native-request-context';
  const requestContext=page.locator('.wb-call-inspector .wb-request-context');
  await expect(requestContext).toContainText('请求文档：已捕获');
  for(let n=0;n<20&&await requestContext.getByRole('button',{name:'加载后续内容'}).count();n++){
    await requestContext.getByRole('button',{name:'加载后续内容'}).click();
    await expect(requestContext.getByRole('button',{name:'刷新上下文'})).toBeEnabled();
  }
  const hasToolDefinition=await requestContext.locator('.wb-context-entry pre').evaluateAll((elements,code)=>{
    const contains=value=>value&&typeof value==='object'&&((code?value.name==='exec'&&value.type==='custom'&&value.format&&typeof value.format==='object':value.name==='exec_command'&&value.parameters&&typeof value.parameters==='object')||Object.values(value).some(contains));
    return elements.some(element=>{try{return contains(JSON.parse(element.textContent));}catch{return false;}});
  },code);
  stage='native-tool-definition';
  assert.ok(hasToolDefinition,'the actual native custom format or function schema should be readable with its source');
  await expect(requestContext).toContainText('缺失值不计为零');
  assert.ok(!(await requestContext.textContent()).includes('synthetic-r2-tools'));
  await screenshot('context');
  await closeCalls(page);await expect(parameters).toHaveAttribute('open','');
  assert.equal(await terminal.evaluate(element=>element===document.querySelector('.xterm-helper-textarea')),true);
  stage='snapshot-refresh';await page.reload();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');await expect(tools).toHaveCount(3);
  for(let index=0;index<3;index++)await expect(tools.nth(index)).toHaveAttribute('data-execution',code?'result_observed':index===2?'failed':'succeeded');
  const after=await readRun(page, '/run');assert.equal(after.processId,before.processId);assert.equal(after.runEpoch,before.runEpoch);
  for(const width of [1024,736,320]){await page.setViewportSize({width,height:900});await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);}
  await page.setViewportSize({width:1600,height:1000});
  const outputDetails=tools.nth(2).locator('.wb-tool-result details');
  if(await outputDetails.getAttribute('open')===null)await outputDetails.locator('summary').press('Enter');
  await screenshot('complete');assert.equal(errors,0);
  assert.ok(sawTrueColor,'native CLI should emit rich color through the production PTY filter');
  await input.press('Control+d');await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');
  report({stage:'complete',codeMode:code,toolCards:3,userMessages:1,modelMessages:4,sameCliProcess:true,pageErrors:errors,trueColor:sawTrueColor,staticWelcome:true,nativeRequestDefinition:hasToolDefinition,browser:browser.version()});await close(0);
})().catch(async()=>{await screenshot('failed').catch(()=>{});report({stage:'failed',check:stage,codeMode:code,reason:'assertion (private URL/output suppressed)'});close(1);});
