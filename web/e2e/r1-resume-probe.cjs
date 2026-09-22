const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage='launch';
const resumed=process.env.WORKBENCH_PROBE_RESUMED==='true';
const report=value=>process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(()=>{report({stage:'failed',check:stage,reason:'deadline'});close(1);},55000).unref();
const text=()=>page.locator('.xterm-rows').innerText();
const input=()=>page.locator('.xterm-helper-textarea');
(async()=>{
  browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  page=await browser.newPage({viewport:{width:1440,height:900}});page.setDefaultTimeout(15000);
  let errors=0;page.on('pageerror',()=>errors++);
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  stage='native-ready';let themed=false,trusted=false,ready=false;
  for(let n=0;n<100;n++){
    const screen=await text();
    if(!themed&&/Choose your style|Select a theme/.test(screen)){await input().press('Enter');themed=true;}
    else if(!trusted&&/Do you trust|Do you want to work/.test(screen)){await input().press('Enter');trusted=true;}
    else if(screen.includes('OpenAI Codex')&&screen.includes('›')){ready=true;break;}
    await page.waitForTimeout(150);
  }
  assert.ok(ready);
  if(resumed){await expect.poll(text).toContain('R1_RESUME_HISTORY');}
  stage='no-automatic-replay';
  await page.waitForTimeout(400);
  const snapshot=await page.evaluate(async()=>(await fetch('/workbench/v1/live/snapshot')).json());
  assert.equal(snapshot.requests.filter(request=>request.purpose==='conversation').length,0);
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(0);
  stage='new-native-submission';
  const prompt=resumed?'R1_RESUME_NEXT：继续刚才的合成会话。':'R1_RESUME_FIRST：记住这个合成会话标记。';
  await input().evaluate((element,value)=>{const transfer=new DataTransfer();transfer.setData('text/plain',value);element.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true}));},prompt);
  await expect.poll(text).toContain(prompt);await input().press('Enter');
  const expected=resumed?'R1_RESUME_DONE':'R1_RESUME_HISTORY';
  await expect(page.locator('.wb-message-list')).toContainText(expected);
  await expect(page.locator('.wb-message-list [data-role="user"]')).toHaveCount(1);
  await expect(page.locator('.wb-message-list [data-role="assistant"]')).toHaveCount(1);
  await expect.poll(text).toContain(expected);
  if(process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.${resumed?'resume':'seed'}.png`,fullPage:true});
  stage='native-exit';await input().press('Control+d');
  await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');assert.equal(errors,0);
  report({stage:'complete',resumed,historyVisible:resumed,noAutomaticReplay:true,userMessages:1,modelMessages:1,pageErrors:errors,browser:browser.version()});
  await close(0);
})().catch(async()=>{
  if(page&&process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.failed.png`,fullPage:true}).catch(()=>{});
  report({stage:'failed',check:stage,resumed,reason:'assertion (private URL/output suppressed)'});close(1);
});
