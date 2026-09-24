// Real product homepage and installed native CLI. All model/history data is synthetic.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const projectless = process.env.WORKBENCH_PROBE_PROJECTLESS === '1';
// Change only the synthetic SessionMeta after the real CLI has exited. Never use
// the developer's native home: the Rust harness injects and owns every path here.
async function markSyntheticDesktopSession() {
  const root = await fs.realpath(process.env.WORKBENCH_PROBE_ISOLATED_ROOT);
  const home = await fs.realpath(process.env.CODEX_HOME);
  const project = await fs.realpath(process.env.WORKBENCH_PROBE_PROJECT);
  assert.equal(await fs.realpath(process.env.HOME), root);
  assert.equal(await fs.realpath(process.env.USERPROFILE), root);
  assert.equal(home, path.join(root, 'native'));
  assert.equal(project, path.join(root, 'Documents/Codex/2026-09-23/new-chat'));
  let changed = 0;
  async function visit(directory) {
    for (const item of await fs.readdir(directory, {withFileTypes:true})) {
      assert.equal(item.isSymbolicLink(), false);
      const file = path.join(directory, item.name);
      if (item.isDirectory()) await visit(file);
      else if (item.isFile() && item.name.endsWith('.jsonl')) {
        const bytes = await fs.readFile(file, 'utf8');
        const newline = bytes.indexOf('\n');
        assert.ok(newline > 0);
        const meta = JSON.parse(bytes.slice(0, newline));
        if (meta.type !== 'session_meta' || meta.payload.cwd !== project) continue;
        meta.payload.originator = 'Codex Desktop';
        await fs.writeFile(file, JSON.stringify(meta) + bytes.slice(newline));
        changed++;
      }
    }
  }
  await visit(path.join(home, 'sessions'));
  assert.equal(changed, 1);
}
let stage='home', page;
setTimeout(()=>{process.stdout.write(JSON.stringify({stage:'failed',check:stage,reason:'deadline'})+'\n');close(1);},95000).unref();
const terminal=()=>page.locator('.xterm-helper-textarea');
const screen=()=>page.locator('.xterm-rows').innerText();
const app=()=>page.evaluate(async()=>(await(await fetch('/workbench/v1/application')).json()));
async function nativeReady(){
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  for(let n=0;n<80;n++){
    const text=await screen();
    if(/Choose your style|Select a theme|Do you trust|Do you want to work|Trust this folder/.test(text)){await terminal().press('Enter');}
    else if(text.includes('OpenAI Codex')&&text.includes('›'))return;
    await page.waitForTimeout(150);
  }
  throw new Error('native not ready');
}
async function paste(value){await terminal().evaluate((el,value)=>{const data=new DataTransfer();data.setData('text/plain',value);el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true}));},value);}
(async()=>{
  const browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true});
  const context=await browser.newContext({viewport:{width:1440,height:900}});
  let errors=0,starts=0,conversationInputs=0;const scoped=[];
  context.on('page', page=>{
  page.setDefaultTimeout(10000);
  page.on('pageerror',()=>errors++);
  page.on('response',async response=>{
    if(new URL(response.url()).pathname==='/workbench/v1/launch-targets'&&!response.ok()){
      const body=await response.json().catch(()=>({}));
      const code=body.error?.code;
      process.stdout.write(JSON.stringify({stage:'launch-target-error',status:response.status(),code:typeof code==='string'&&/^[a-z_]{1,80}$/.test(code)?code:'unknown'})+'\n');
    }
  });

  page.on('request',r=>{const path=new URL(r.url()).pathname;if(path==='/workbench/v1/runs'&&r.method()==='POST')starts++;if(/\/(live|terminal|requests|workspace|history|settings)(\/|$)/.test(path)&&!path.includes('/library/')&&!path.includes('/application/'))scoped.push(path);});
  page.on('websocket',socket=>socket.on('framesent',({payload})=>{try{const frame=JSON.parse(payload);if(frame.command?.type==='input'){const text=Buffer.from(frame.command.data,'base64').toString();if(text!=='\x1b[I'&&text!=='\x1b[O')conversationInputs++;}}catch{}}));
  });
  page=await context.newPage();
  await page.goto(process.env.WORKBENCH_PROBE_URL);await expect(page.getByRole('heading',{name:'全部历史',exact:true})).toBeVisible();assert.equal((await app()).runs.length,0);
  stage='folder-picker';
  let picks=0;
  // Stub the OS result only; target validation and all subsequent launches use the real server.
  await page.route('**/workbench/v1/application/pick-directory',async route=>{
    assert.equal(route.request().method(),'POST');
    assert.deepEqual(route.request().postDataJSON(),{instanceId:(await app()).instanceId});
    await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({path:++picks===1?null:process.env.WORKBENCH_PROBE_PROJECT})});
  });
  const historyHeading=page.getByRole('heading',{name:'全部历史',exact:true});
  const beforePicker=await historyHeading.boundingBox();
  await page.getByRole('button',{name:'打开其他目录',exact:true}).click();
  await expect.poll(()=>picks).toBe(1);
  await expect(page.getByRole('button',{name:'打开其他目录',exact:true})).toBeEnabled();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  assert.deepEqual(await historyHeading.boundingBox(),beforePicker);assert.equal(starts,0);
  await page.getByRole('button',{name:'打开其他目录',exact:true}).click();
  await expect(page.getByRole('dialog',{name:'打开项目',exact:true})).toBeVisible();
  await expect(page.getByRole('button',{name:'开始新对话',exact:true})).toBeEnabled();
  await expect(page.locator('.wb-launch-native')).toHaveCount(0);
  assert.equal(picks,2);assert.equal(starts,0);assert.equal(conversationInputs,0);
  await page.keyboard.press('Escape');await expect(page.getByRole('dialog')).toHaveCount(0);
  await page.unroute('**/workbench/v1/application/pick-directory');
  stage='manual-path';
  const manual=page.getByRole('button',{name:'输入路径',exact:true});
  await manual.click();await expect(page.getByLabel('项目目录',{exact:true})).toBeFocused();
  await page.keyboard.press('Escape');await expect(manual).toBeFocused();
  await manual.click();await page.getByLabel('项目目录',{exact:true}).fill(process.env.WORKBENCH_PROBE_PROJECT);
  await page.getByLabel('项目目录',{exact:true}).press('Enter');await expect(page.getByRole('button',{name:'开始新对话'})).toBeEnabled();assert.equal((await app()).runs.length,0);assert.equal(conversationInputs,0);
  for(const width of [1024,736,390,320]){await page.setViewportSize({width,height:900});assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));if(process.env.WORKBENCH_PROBE_SCREENSHOT&&[1024,390].includes(width))await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.launch-${width}.png`});}
  await page.setViewportSize({width:1440,height:900});
  stage='lost-launch-ack';
  await page.route('**/workbench/v1/runs',async route=>{if(route.request().method()==='POST'){const response=await route.fetch();assert.equal(response.status(),202);await route.abort();}else await route.continue();});
  await page.getByRole('button',{name:'开始新对话'}).click();await expect(page.getByRole('button',{name:'查询启动结果'})).toBeVisible();assert.equal(starts,1);
  await page.unroute('**/workbench/v1/runs');await page.reload();
  await expect(page.getByRole('link',{name:'打开工作台',exact:true})).toBeVisible();
  const recoveredPage=page.waitForEvent('popup');
  await page.getByRole('link',{name:'打开工作台',exact:true}).click();
  page=await recoveredPage;
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');assert.equal(starts,1);assert.equal(conversationInputs,0);
  const first=(await app()).runs[0];assert.equal(new URL(page.url()).searchParams.get('run'),first.runId);
  stage='new-native-turn';await nativeReady();
  await paste('R6_E_FIRST：请记住本次合成会话。');await terminal().press('Enter');await expect(page.locator('.wb-message-list')).toContainText('R6_E_HISTORY');await expect.poll(screen).toContain('R6_E_HISTORY');
  stage='return-keeps-cli';await page.getByRole('link',{name:'← 全部历史'}).click();assert.equal((await app()).runs[0].cliPid,first.cliPid);assert.equal((await app()).runs[0].state,'running');
  stage='index-running-session';
  await page.getByRole('button',{name:'更新历史',exact:true}).click();
  await expect.poll(async()=>{const value=await page.evaluate(async()=>(await(await fetch('/workbench/v1/library/entries?kind=native')).json()));return value.records?.length||0;}).toBeGreaterThan(0);
  await page.getByLabel('历史来源筛选').selectOption('default-native');await expect(page.locator('.wb-library-entry')).toHaveCount(1);await page.locator('.wb-library-entry').click();
  stage='reenter-running-session';
  await page.getByRole('button',{name:'继续此会话',exact:true}).click();await expect(page.getByRole('button',{name:'进入工作台',exact:true})).toBeVisible();await page.getByRole('button',{name:'进入工作台',exact:true}).click();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');if(await page.getByRole('button',{name:'在此输入',exact:true}).isVisible())await page.getByRole('button',{name:'在此输入',exact:true}).click();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');assert.equal(starts,1);
  stage='native-stop';await terminal().press('Control+d');await expect(page.locator('.wb-terminal-status')).toHaveText('已结束');await page.getByRole('dialog',{name:'本次运行已结束',exact:true}).getByRole('link',{name:'返回首页',exact:true}).click();
  if(projectless){
    stage='classify-projectless-fixture';
    await expect.poll(async()=>(await app()).runs.find(run=>run.runId===first.runId)?.state).toBe('stopped');
    await markSyntheticDesktopSession();
    await page.getByRole('button',{name:'更新历史',exact:true}).click();
    await expect.poll(async()=>{
      const result=await page.evaluate(async()=>(await(await fetch('/workbench/v1/library/entries?kind=native')).json()));
      const item=result.records?.[0];
      return item&&[item.projectBasis,item.projectId,item.projectPath,item.recordedCwd];
    }).toEqual(['desktop_generated',null,null,process.env.WORKBENCH_PROBE_PROJECT]);
    await page.reload();
    await page.getByLabel('历史来源筛选').selectOption('default-native');
    await page.getByRole('button',{name:/^未归属项目/}).click();
    await expect(page.locator('.wb-library-entry')).toHaveCount(1);
    await expect(page.locator('.wb-library-entry')).toContainText('未归属项目');
    await expect(page.getByRole('button',{name:'开始新对话',exact:true})).toHaveCount(0);
  }
  stage='resume-from-history';await page.getByLabel('历史来源筛选').selectOption('default-native');await page.getByRole('button',{name:'更新历史',exact:true}).click();await expect(page.locator('.wb-library-entry')).toHaveCount(1);await page.locator('.wb-library-entry').click();await page.getByRole('button',{name:'继续此会话',exact:true}).click();
  const panel=page.getByRole('dialog',{name:'继续此会话',exact:true});await expect(panel.getByRole('button',{name:'继续此会话',exact:true})).toBeEnabled();await expect(panel.locator('.wb-launch-native summary')).toHaveText('所选会话的 Codex 数据目录');const beforeResumeInputs=conversationInputs;await panel.getByRole('button',{name:'继续此会话',exact:true}).click();
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');assert.equal(conversationInputs,beforeResumeInputs);await nativeReady();await expect.poll(screen).toContain('R6_E_HISTORY');
  const second=(await app()).runs[0];assert.notEqual(second.runId,first.runId);assert.notEqual(second.cliPid,first.cliPid);assert.ok(second.resume);assert.equal(second.projectPath,process.env.WORKBENCH_PROBE_PROJECT);assert.equal(starts,2);
  const snapshot=await page.evaluate(async id=>(await(await fetch(`/workbench/v1/runs/${id}/live/snapshot`)).json()),second.runId);assert.equal(snapshot.requests.filter(r=>r.purpose==='conversation').length,0);
  await paste('R6_E_NEXT：继续刚才的会话。');await terminal().press('Enter');await expect(page.locator('.wb-message-list')).toContainText('R6_E_DONE');
  stage='run-routing';await page.getByRole('button',{name:'文件',exact:true}).click();await page.getByRole('treeitem').filter({hasText:'R6-check.txt'}).click();await expect(page.getByRole('region',{name:'代码内容',exact:true})).toContainText('Synthetic R6 source');
  assert.ok(scoped.length>5);assert.ok(scoped.every(path=>/^\/workbench\/v1\/runs\/[a-f0-9-]+\//.test(path)), 'Run clients must use explicit Run paths');
  if(process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.workbench.png`});
  assert.equal(errors,0);process.stdout.write(JSON.stringify({stage:'passed',starts,viewports:[1024,736,390,320],nativePickerApiStub:true,pickerCancelKeepsLayout:true,manualEnterAndEscape:true,lostAckRecovered:true,sameNativeThread:true,projectlessResume:projectless,recordedCwdPreserved:projectless,zeroAutomaticInput:true,runScopedClients:true,pageErrors:errors,browser:browser.version()})+'\n');await close(0);
})().catch(async()=>{if(page&&process.env.WORKBENCH_PROBE_SCREENSHOT)await page.screenshot({path:`${process.env.WORKBENCH_PROBE_SCREENSHOT}.failed.png`}).catch(()=>{});process.stdout.write(JSON.stringify({stage:'failed',check:stage})+'\n');close(1);});
