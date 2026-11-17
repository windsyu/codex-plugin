// Production frontend acceptance with isolated system Chrome and synthetic API/SSE/WS.
// node web/e2e/product-design-probe.mjs [--prepare|--history-only]
// This is visual/capability coverage; it does not validate real auth, E2EE or CLI.
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createRequire} from 'node:module';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import {createServer as createNetServer} from 'node:net';
import {createServer} from 'vite';
import {expect} from '@playwright/test';
import {createRun} from '../../scripts/lib/artifacts.mjs';
import {productFixture,runId,savedId,historyProjects,historyGroups} from './product-design-fixture.mjs';
const require=createRequire(import.meta.url);
const {chromium,close}=require('./browser-lifecycle.cjs');
const {trackDocumentSockets}=require('./secure-probe-utils.cjs');
const repo=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
assert.ok(process.argv.length===2||(process.argv.length===3&&['--prepare','--history-only'].includes(process.argv[2])),'Only --prepare or --history-only is accepted');
const historyOnly=process.argv[2]==='--history-only';
if(process.argv[2]==='--prepare'){
 assert.equal(runId.length,36);assert.equal(savedId.length,36);
 for(const file of ['web/src/workbench/main.tsx','web/src/workbench/App.tsx','web/src/workbench/WorkbenchFrame.tsx','relay/static/portal.js'])assert.ok(fs.existsSync(path.join(repo,file)),file);
 console.log('Production probe and synthetic fixture are prepared. Run without --prepare after session.css is ready.');
 process.exit(0);
}
const artifact=createRun(repo,{label:historyOnly?'production-history-classification-system-chrome':'production-commercial-ui-system-chrome'});
const runtime=path.join(artifact.path,'isolated-browser');fs.mkdirSync(runtime,{mode:0o700});
const fixture=productFixture(repo);fixture.state.historyEnabled=historyOnly;
const sourceManifest=()=>{
 const files=[];
 const visit=directory=>{for(const name of fs.readdirSync(path.join(repo,directory))){const relative=path.join(directory,name);if(fs.statSync(path.join(repo,relative)).isDirectory())visit(relative);else files.push(relative);}};
 visit('web/src');files.push('web/index.html','web/workbench.html','web/vite.config.ts','relay/static/index.html','relay/static/portal.css','relay/static/portal.js','web/e2e/product-design-probe.mjs','web/e2e/product-design-fixture.mjs');
 return Object.fromEntries(files.sort().map(file=>[file,crypto.createHash('sha256').update(fs.readFileSync(path.join(repo,file))).digest('hex')]));
};
const sourcesBefore=sourceManifest();fs.writeFileSync(path.join(artifact.path,'source-manifest.json'),JSON.stringify(sourcesBefore,null,2)+'\n');
const result={scope:'Production Preact/portal HTML/CSS/JS with synthetic loopback API/SSE/WS. No real CLI, accounts, remote grants or external services.',checks:[],failures:[],pageErrors:[],resources:[],externalRequests:[],screenshots:[],geometry:[],limitations:['RemoteOwner capability navigation is exercised through the genuine remoteOwner response flag, without a /g secure transport. Real encrypted relay and authentication require existing integration probes.','Terminal input is echoed by a synthetic WS server; native PTY editing and real CLI draft behavior are not tested.','Mobile checks use system Chrome viewports; real phones, TLS and touch keyboards are not tested.']};
let server,browser,context,page,origin,stage='startup',success=false;
const widths=[1440,1024,736,390,320];
const settle=async()=>{
 await page.evaluate(async()=>{await document.fonts.ready;await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));});
 if(await page.locator('.wb-terminal[data-owned=true] .xterm-screen').isVisible()){
  // Native xterm applies server resize state asynchronously after the DOM layout.
  // Wait for its canvas dimensions rather than recording a transient old width.
  await expect.poll(()=>page.evaluate(()=>{const screen=document.querySelector('.xterm-screen')?.getBoundingClientRect(),host=document.querySelector('.wb-xterm')?.getBoundingClientRect();return !!screen&&!!host&&screen.width<=host.width+1&&screen.height<=host.height+1;}),{timeout:7000}).toBe(true);
 }
 await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
};
const capture=async name=>{
 await settle();
 await page.evaluate(async()=>{await document.fonts.ready;await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));});
 const file=name+'.png';await page.screenshot({path:path.join(artifact.path,file)});result.screenshots.push(file);
};
const bounds=async name=>{
 await settle();
 const geometry=await page.evaluate(()=>({width:innerWidth,height:innerHeight,scrollWidth:document.documentElement.scrollWidth,bodyWidth:document.body.scrollWidth,overflow:[...document.querySelectorAll('body *')].filter(element=>{const r=element.getBoundingClientRect(),style=getComputedStyle(element);return r.width&&r.height&&style.visibility!=='hidden'&&(r.right>innerWidth+1||r.left< -1);}).slice(0,8).map(e=>({tag:e.tagName,class:e.className?.baseVal??e.className,right:e.getBoundingClientRect().right,left:e.getBoundingClientRect().left}))}));
 result.geometry.push({name,...geometry});assert.ok(geometry.scrollWidth<=geometry.width,`${name}: document scrollWidth ${geometry.scrollWidth} > ${geometry.width}`);assert.ok(geometry.bodyWidth<=geometry.width,`${name}: body scrollWidth ${geometry.bodyWidth} > ${geometry.width}`);
};
const check=async(name,action)=>{stage=name;try{await action();result.checks.push(name);}catch(error){result.failures.push({stage:name,message:String(error.message).slice(0,1600)});if(page&&!page.isClosed())await capture('failure-'+name.replace(/[^a-z0-9-]/gi,'-')).catch(()=>{});}};
const goto=async(url,width=1440)=>{await page.setViewportSize({width,height:960});await page.goto(origin+url,{waitUntil:'domcontentloaded'});await expect(page.locator('.wb-product')).toBeVisible();};
const runURL=`/runs/${runId}/chat`;
try{
 // Vite 5 treats port 0 as its default. Reserve a kernel-selected free port,
 // then use strictPort so a race fails instead of sharing another task's port.
 const reservation=createNetServer();await new Promise((resolve,reject)=>{reservation.once('error',reject);reservation.listen(0,'127.0.0.1',resolve);});
 const dedicatedPort=reservation.address().port;await new Promise(resolve=>reservation.close(resolve));
 server=await createServer({root:path.join(repo,'web'),cacheDir:path.join(artifact.path,'vite-cache'),configFile:path.join(repo,'web/vite.config.ts'),plugins:[fixture.plugin],server:{host:'127.0.0.1',port:dedicatedPort,strictPort:true},logLevel:'error'});
 await server.listen();result.port=dedicatedPort;origin=`http://127.0.0.1:${server.httpServer.address().port}`;
 browser=await chromium.launch({executablePath:process.env.WORKBENCH_TEST_CHROME||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true,args:['--no-proxy-server'],env:{...process.env,HOME:runtime,USERPROFILE:runtime,CODEX_HOME:path.join(runtime,'codex-home'),TMPDIR:runtime}});
 result.browser=browser.version();context=await browser.newContext({viewport:{width:1440,height:960},deviceScaleFactor:1});
 await context.route('**/*',route=>{const url=new URL(route.request().url());if(url.origin===origin)return route.continue();result.externalRequests.push(url.origin+url.pathname);return route.abort();});
 page=await context.newPage();page.setDefaultTimeout(7000);
 page.on('pageerror',error=>result.pageErrors.push({stage,message:error.message}));
 page.on('response',response=>{if(response.status()>=400)result.resources.push({stage,path:new URL(response.url()).pathname,status:response.status()});});
 page.on('requestfailed',request=>{if(request.failure()?.errorText!=='net::ERR_ABORTED')result.resources.push({stage,path:new URL(request.url()).pathname,error:request.failure()?.errorText});});
 const sockets=await trackDocumentSockets(page);
 if(!historyOnly){
 await check('local-home-all-widths',async()=>{
  fixture.state.mode='local';await goto('/');await expect(page.getByRole('heading',{name:'从这里，继续创作。'})).toBeVisible();await expect(page.locator('.wb-home-run-card')).toHaveCount(1);
  await expect(page.locator('.wb-product-nav').getByRole('link',{name:'连接管理',exact:true})).toBeVisible();
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('home-'+width);await capture('home-'+width);}
 });
 await check('run-continuous-reading-all-widths',async()=>{
  await goto(runURL);await expect(page.locator('.wb-model-message')).toContainText('SSE 中间内容已到达');await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  await expect(page.locator('.wb-message-list')).toContainText('请核对商业界面');await expect(page.locator('.wb-model-message')).toContainText('正在接收');
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('run-reading-'+width);await capture('run-reading-'+width);}
  await page.setViewportSize({width:1440,height:960});await page.locator('.wb-message-list').evaluate(element=>{element.scrollTop=0;});await expect(page.locator('.wb-model-message h2')).toBeInViewport();await capture('run-reading-start-1440');
 });
 await check('terminal-dock-collapse-draft-and-mobile-dom-identity',async()=>{
  await goto(runURL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const baseline=fixture.state.input.length,connections=fixture.state.sockets,generation=sockets.generation;
  await page.evaluate(()=>{window.__productDesignTerminal=document.querySelector('.xterm');});
  await page.locator('.xterm-helper-textarea').focus();await page.keyboard.type('SYNTHETIC_DRAFT_NOT_SUBMITTED');
  await expect.poll(()=>fixture.state.input.slice(baseline).join('')).toBe('SYNTHETIC_DRAFT_NOT_SUBMITTED');
  const same=async()=>{assert.equal(await page.evaluate(()=>window.__productDesignTerminal===document.querySelector('.xterm')),true);assert.equal(fixture.state.sockets,connections);assert.equal(sockets.generation,generation);assert.equal(fixture.state.input.slice(baseline).join(''),'SYNTHETIC_DRAFT_NOT_SUBMITTED');};
  await page.getByRole('button',{name:'切换为底部终端'}).click();await expect(page.locator('.wb-shell')).toHaveClass(/wb-dock-bottom/);await same();await capture('run-terminal-bottom');
  await page.getByRole('button',{name:'切换为右侧终端'}).click();await same();
  await page.getByRole('button',{name:'收起终端',exact:true}).click();await expect(page.locator('#wb-session-terminal')).toBeHidden();await same();
  await page.locator('.wb-session-actions').getByRole('button',{name:'终端',exact:true}).click();await expect(page.locator('#wb-session-terminal')).toBeVisible();await same();
  for(const width of [736,390,320]){
   await page.setViewportSize({width,height:960});await page.getByRole('tab',{name:'终端',exact:true}).click();await expect(page.locator('.wb-xterm')).toBeVisible();await same();await bounds('run-terminal-'+width);await capture('run-terminal-'+width);
   await page.getByRole('tab',{name:'阅读',exact:true}).click();await expect(page.locator('.wb-message-list')).toBeVisible();await same();
  }
 });
 await check('run-files-search-git-history',async()=>{
  await goto(runURL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');const connections=fixture.state.sockets;
  await page.evaluate(()=>{window.__productDesignTerminal=document.querySelector('.xterm');});
  const tools=page.getByRole('navigation',{name:'会话功能'});
  await tools.getByRole('link',{name:'文件',exact:true}).click();await expect(page.getByRole('treeitem',{name:'README.md'})).toBeVisible();await page.getByRole('treeitem',{name:'README.md'}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toContainText('这里是只读文件夹具');await capture('run-file-reader');
  await tools.getByRole('link',{name:'搜索',exact:true}).click();await page.getByLabel('搜索内容',{exact:true}).fill('designFixture');await page.locator('.wb-search-form').getByRole('button',{name:'搜索',exact:true}).click();await expect(page.locator('.wb-search-hit')).toContainText('designFixture');await capture('run-search');
  await tools.getByRole('link',{name:'更改',exact:true}).click();await expect(page.locator('.wb-git-branch')).toContainText('synthetic/design');await page.getByRole('button',{name:'查看未暂存差异 README.md（修改）',exact:true}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toContainText('updated synthetic line');await capture('run-git');
  await tools.getByRole('link',{name:'历史',exact:true}).click();await expect(page.locator('.wb-history-open')).toHaveCount(1);await page.locator('.wb-history-open').click();await expect(page.locator('.wb-saved-reading')).toContainText('连续阅读示例');await expect(page.getByText('历史内容只读。右侧终端始终属于当前运行，输入会发送给当前 CLI。',{exact:true})).toBeVisible();await capture('run-history');
  assert.equal(await page.evaluate(()=>window.__productDesignTerminal===document.querySelector('.xterm')),true);assert.equal(fixture.state.sockets,connections);
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('history-'+width);}
  await page.setViewportSize({width:320,height:960});await tools.getByRole('link',{name:'文件',exact:true}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toBeVisible();await bounds('file-reader-320');await capture('run-file-reader-320');
 });
 await check('mobile-hidden-workspace-pauses-polling-and-resumes',async()=>{
  fixture.state.mode='local';await goto(runURL,390);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const connections=fixture.state.sockets;await page.evaluate(()=>{window.__productDesignTerminal=document.querySelector('.xterm');});
  const tools=page.getByRole('navigation',{name:'会话功能'}),workspaceRequests=()=>fixture.state.requests.filter(request=>request.path.includes('/workspace/'));
  await tools.getByRole('link',{name:'文件',exact:true}).click();await page.getByRole('treeitem',{name:'README.md'}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toContainText('这里是只读文件夹具');
  await page.getByRole('tab',{name:'终端',exact:true}).click();await expect(page.locator('#wb-session-reading')).toBeHidden();await settle();let count=workspaceRequests().length;
  await page.waitForTimeout(5600);assert.equal(workspaceRequests().length,count,'Hidden mobile files/tree must not poll after the 5s interval');
  await page.getByRole('tab',{name:'阅读',exact:true}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toBeVisible();await expect.poll(()=>workspaceRequests().length).toBeGreaterThan(count);
  await tools.getByRole('link',{name:'更改',exact:true}).click();await expect(page.locator('.wb-git-branch')).toContainText('synthetic/design');await page.getByRole('button',{name:'查看未暂存差异 README.md（修改）',exact:true}).click();await expect(page.getByRole('region',{name:'文件阅读'})).toContainText('updated synthetic line');
  await page.getByRole('tab',{name:'终端',exact:true}).click();await expect(page.locator('#wb-session-reading')).toBeHidden();await settle();count=workspaceRequests().length;
  await page.waitForTimeout(5600);assert.equal(workspaceRequests().length,count,'Hidden mobile Git and selected diff must not poll');
  await page.setViewportSize({width:1024,height:960});await expect(page.getByRole('region',{name:'文件阅读'})).toBeVisible();await expect.poll(()=>workspaceRequests().length).toBeGreaterThan(count);await bounds('workspace-restored-wide');await capture('mobile-workspace-restored-wide');
  assert.equal(await page.evaluate(()=>window.__productDesignTerminal===document.querySelector('.xterm')),true);assert.equal(fixture.state.sockets,connections);
  await page.getByRole('button',{name:'关闭辅助面板',exact:true}).focus();await page.keyboard.press('Escape');await expect(page.locator('.wb-session-context')).toBeHidden();await expect(tools.getByRole('link',{name:'更改',exact:true})).toBeFocused();
 });
 await check('keyboard-separator-drawer-and-reduced-motion',async()=>{
  await goto(runURL);await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned','true');
  const separator=page.getByRole('separator',{name:'调整终端宽度',exact:true});await separator.focus();const prior=Number(await separator.getAttribute('aria-valuenow'));await page.keyboard.press('ArrowLeft');await expect(separator).toHaveAttribute('aria-valuenow',String(prior+16));
  await page.keyboard.press('Home');await expect(separator).toHaveAttribute('aria-valuenow',await separator.getAttribute('aria-valuemin'));await page.keyboard.press('End');await expect(separator).toHaveAttribute('aria-valuenow',await separator.getAttribute('aria-valuemax'));await bounds('keyboard-separator');
  await page.setViewportSize({width:390,height:960});const menu=page.getByRole('button',{name:'展开项目与会话',exact:true});await menu.click();const drawer=page.getByRole('dialog',{name:'项目与会话导航',exact:true});await expect(drawer).toBeVisible();await expect(drawer.getByRole('button',{name:'收起项目与会话',exact:true})).toBeFocused();await page.keyboard.press('Escape');await expect(drawer).toBeHidden();await expect(menu).toBeFocused();
  await page.emulateMedia({reducedMotion:'reduce'});assert.equal(await page.evaluate(()=>matchMedia('(prefers-reduced-motion: reduce)').matches),true);assert.equal(await page.locator('.wb-message-list').evaluate(element=>getComputedStyle(element).scrollBehavior),'auto');await bounds('reduced-motion-390');await capture('reduced-motion-390');await page.emulateMedia({reducedMotion:'no-preference'});
 });
 await check('local-connection-management-all-widths',async()=>{
  fixture.state.mode='local';await goto('/relay');await expect(page.getByRole('heading',{name:'连接管理',exact:true})).toBeVisible();await expect(page.getByRole('region',{name:'公网接入设置'})).toContainText('未绑定');
  await expect(page.getByRole('button',{name:'登录并开启公网接入',exact:true})).toBeEnabled();
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('connection-'+width);await capture('connection-'+width);}
 });
 await check('remote-owner-capability-navigation',async()=>{
  fixture.state.mode='remote';await goto('/');await expect(page.locator('.wb-product')).toHaveClass(/is-remote/);await expect(page.locator('.wb-product-nav').getByRole('link',{name:'连接管理',exact:true})).toHaveCount(0);await expect(page.locator('.wb-project-navigation').getByRole('link',{name:/远程连接/})).toHaveCount(0);await expect(page.locator('.wb-new-project')).toBeVisible();await capture('remote-owner-home');
  const start=fixture.state.requests.length;await goto(runURL);await expect(page.locator('.wb-model-message')).toContainText('SSE 中间内容已到达');await expect(page.getByRole('link',{name:'本地局域网',exact:true})).toBeVisible();
  await page.getByRole('button',{name:'会话选项',exact:true}).count();
  await page.locator('summary[aria-label="会话选项"]').click();await expect(page.getByRole('link',{name:'运行设置',exact:true})).toBeVisible();await page.locator('summary[aria-label="会话选项"]').click();
  await bounds('remote-owner-run');await capture('remote-owner-run');
  await page.locator('.wb-product-nav').getByRole('link',{name:'连接管理',exact:true}).click();await expect(page.getByRole('region',{name:'本地局域网接入'})).toBeVisible();await expect(page.getByRole('button',{name:'开启设备访问',exact:true})).toBeEnabled();await capture('remote-owner-run-lan');
  assert.ok(!fixture.state.requests.slice(start).some(request=>request.path.startsWith('/workbench/v1/application/relay')),'RemoteOwner must not query the local connector');
 });
 await check('paired-run-capability-navigation',async()=>{
  fixture.state.mode='paired';const requestStart=fixture.state.requests.length;await goto(runURL);await expect(page.locator('.wb-model-message')).toContainText('SSE 中间内容已到达');
  await expect(page.locator('.wb-new-project')).toHaveCount(0);await expect(page.getByRole('link',{name:'全部历史',exact:true})).toHaveCount(0);await expect(page.getByRole('link',{name:'连接管理',exact:true})).toHaveCount(0);await expect(page.locator('.wb-navigation-empty')).toContainText('仅访问当前运行');
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('paired-run-'+width);}
  await capture('paired-run-320');const requests=fixture.state.requests.slice(requestStart);assert.ok(requests.every(item=>item.path==='/workbench/v1/application'||item.path.startsWith(`/workbench/v1/runs/${runId}/`)),'PairedRun must not query application/library/connector outside its scope');
 });
 await check('portal-login-five-states-password-all-widths',async()=>{
  fixture.state.portalSignedIn=false;fixture.state.portalForced=false;await page.setViewportSize({width:1440,height:960});await page.goto(origin+'/login');await expect(page.locator('#signed-out')).toBeVisible();await capture('portal-login');
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('portal-login-'+width);}
  await page.locator('#login input[name=username]').fill('synthetic_user');await page.locator('#login input[name=password]').fill('Synthetic-only-fixture-password');await page.locator('#login button').click();await expect(page.locator('#gateways>li')).toHaveCount(5);await expect(page.locator('#gateways .primary:enabled')).toHaveCount(1);
  for(const state of ['online','offline','awaitingConnection','reauthenticationRequired','versionIncompatible'])await expect(page.locator(`.state[data-state=${state}]`)).toBeVisible();
  for(const width of widths){await page.setViewportSize({width,height:960});await bounds('portal-computers-'+width);await capture('portal-computers-'+width);}
  fixture.state.portalForced=true;await page.goto(origin+'/computers');await expect(page.locator('#security')).toBeVisible();await expect(page.locator('#navigation')).toBeHidden();await expect(page.locator('#forced-change')).toBeVisible();await capture('portal-forced-password-320');await bounds('portal-forced-password-320');
  await page.locator('#change input[name=oldPassword]').fill('Synthetic-only-fixture-password');await page.locator('#change input[name=password]').fill('Synthetic-only-new-fixture-password');await page.locator('#change button').click();await expect(page.locator('#signed-out')).toBeVisible();await expect(page.locator('#status')).toContainText('所有网页与电脑已退出');
 });
 }
 if(historyOnly){
  for(const width of [1440,736,320])await check('history-project-identity-and-conversation-'+width,async()=>{
   await goto('/history',width);await expect(page.getByRole('heading',{name:'全部历史',exact:true,level:1})).toBeVisible();
   const main=page.locator('.wb-library-main'),primary=page.locator('.wb-library-entry-list>.wb-library-session-groups');
   await expect(primary.locator('.wb-library-entry')).toHaveCount(2);
   await expect(primary).not.toContainText('后台审批审查');await expect(page.getByRole('region',{name:'辅助会话',exact:true})).toHaveCount(0);
   for(const group of historyGroups.filter(group=>group.section==='primary')){
    const row=primary.locator(`[data-entry="${group.preferredEntryId}"]`);await expect(row.locator('.wb-library-entry-text>strong')).toHaveText(group.members[0].title);await expect(row.locator('.wb-library-preview')).toContainText(group.members[0].preview.text);await expect(row.locator('.wb-library-entry-project')).toContainText(group.projectPath.replace('/shared-app',''));
   }
   await expect(primary).not.toContainText('AGENTS.md');await expect(primary).not.toContainText('environment_context');await bounds('history-all-'+width);await capture('history-all-'+width);
   const projectNav=page.getByRole('complementary',{name:'项目导航',exact:true});
   const openProjects=async()=>{if(!await projectNav.isVisible()){const toggle=page.locator('.wb-library-project-toggle');await toggle.focus();await page.keyboard.press('Enter');await expect(projectNav).toBeVisible();}};
   await openProjects();
   const scope=page.getByLabel('项目导航范围',{exact:true});await expect(scope).toHaveValue('primary');await expect(scope.locator('option[value=primary]')).toHaveText('主要会话项目');await expect(scope.locator('option[value=all]')).toHaveText('全部记录项目');
   await expect(projectNav.locator('.wb-library-project-item')).toHaveCount(2);await expect(projectNav).not.toContainText('review-only');
   const projectRequests=fixture.state.requests.filter(request=>request.path==='/workbench/v1/library/projects');assert.equal(new URLSearchParams(projectRequests.at(-1).query).get('projectScope'),'primary');assert.equal(new URL(page.url()).searchParams.get('projectScope'),null);
   await scope.selectOption('all');await expect(projectNav.locator('.wb-library-project-item')).toHaveCount(3);await expect(projectNav).toContainText('review-only');assert.equal(new URL(page.url()).searchParams.get('projectScope'),'all');
   await scope.selectOption('primary');await expect(projectNav.locator('.wb-library-project-item')).toHaveCount(2);assert.equal(new URL(page.url()).searchParams.get('projectScope'),null);
   for(const project of historyProjects.slice(0,2))await expect(projectNav.getByRole('button',{name:'shared-app · '+project.path,exact:true})).toContainText(project.path.replace('/shared-app',''));
   await capture('history-project-navigation-'+width);
   for(const project of historyProjects.slice(0,2)){
    await openProjects();const projectButton=projectNav.getByRole('button',{name:'shared-app · '+project.path,exact:true});await projectButton.focus();await page.keyboard.press('Enter');
    await expect(main.getByRole('heading',{name:'shared-app',exact:true,level:1})).toBeVisible();const full=page.locator('.wb-library-selected-path');await expect(full).toHaveAttribute('open','');await expect(full.locator('p').first()).toHaveText(project.path);await expect(primary.locator('.wb-library-entry')).toHaveCount(1);
    assert.equal(new URL(page.url()).searchParams.get('project'),project.projectId);
    // Another filter must preserve project identity and its expanded full path.
    await page.getByLabel('历史来源筛选',{exact:true}).selectOption('synthetic-native');await expect(main.getByRole('heading',{name:'shared-app',exact:true,level:1})).toBeVisible();await expect(full.locator('p').first()).toHaveText(project.path);await expect(primary.locator('.wb-library-entry')).toHaveCount(1);
    await bounds('history-selected-'+project.projectId+'-'+width);await capture('history-selected-'+project.projectId+'-'+width);
   }
   await openProjects();const all=projectNav.getByRole('button',{name:'全部项目',exact:true});await all.focus();await page.keyboard.press('Enter');await expect(main.getByRole('heading',{name:'全部历史',exact:true,level:1})).toBeVisible();await expect(primary.locator('.wb-library-entry')).toHaveCount(2);
   const auxiliary=page.getByRole('button',{name:'辅助会话',exact:true});await auxiliary.focus();await page.keyboard.press('Enter');const region=page.getByRole('region',{name:'辅助会话',exact:true});await expect(region).toContainText(historyGroups[2].members[0].title);await expect(primary).not.toContainText(historyGroups[2].members[0].title);await bounds('history-auxiliary-'+width);await capture('history-auxiliary-'+width);
   await auxiliary.focus();await page.keyboard.press('Enter');await expect(region).toHaveCount(0);
   const entry=primary.locator('.wb-library-entry').first();await entry.focus();await page.keyboard.press('Enter');await expect(page.getByRole('region',{name:'历史会话阅读',exact:true})).toBeVisible();await expect(page.getByLabel('会话正文',{exact:true})).toContainText(historyGroups[0].members[0].title);
   const identity=page.locator('.wb-library-identity');await identity.locator('summary').click();await expect(identity).toContainText('归属依据：按工作目录推断，未取得官方项目关联');await bounds('history-keyboard-reader-'+width);await capture('history-keyboard-reader-'+width);
   // Scope changes while reading must leave all right-hand state untouched and
   // issue no implicit body read, launch, refresh, or input.
   const selected=historyProjects[1],selectedGroup=historyGroups[1];
   const fixed=new URLSearchParams({project:selected.projectId,q:'目录',source:'synthetic-native',section:'auxiliary',entry:selectedGroup.preferredEntryId,revision:selectedGroup.members[0].sourceRevision});
   await goto('/history?'+fixed,width);await expect(main.getByRole('heading',{name:'shared-app',exact:true,level:1})).toBeVisible();await expect(page.getByLabel('会话正文',{exact:true})).toContainText(selectedGroup.members[0].title);
   const beforeURL=new URL(page.url()),bodyBefore=await page.getByLabel('会话正文',{exact:true}).innerText(),bodyPath='/workbench/v1/library/entries/'+beforeURL.searchParams.get('entry');
   const bodyReads=()=>fixture.state.requests.filter(request=>request.path===bodyPath).length;
   const readCount=bodyReads();await openProjects();await scope.selectOption('all');await expect(projectNav.locator('.wb-library-project-item')).toHaveCount(3);
   for(const key of ['project','q','source','section','entry','revision','match'])assert.equal(new URL(page.url()).searchParams.get(key),beforeURL.searchParams.get(key),'Scope must preserve '+key);
   await scope.selectOption('primary');await expect(projectNav.locator('.wb-library-project-item')).toHaveCount(2);
   for(const key of ['project','q','source','section','entry','revision','match'])assert.equal(new URL(page.url()).searchParams.get(key),beforeURL.searchParams.get(key),'Scope must preserve '+key);
   assert.equal(bodyReads(),readCount,'Changing navigation scope must not implicitly reread open source body');assert.equal(await page.getByLabel('会话正文',{exact:true}).innerText(),bodyBefore);

  });
 }
 await check('resource-and-run-scope-invariants',async()=>{
  assert.deepEqual(fixture.state.unexpected,[],'All API/WS calls must be explicitly supported by the synthetic fixture');assert.deepEqual(result.pageErrors,[]);assert.deepEqual(result.externalRequests,[]);
  const unexpectedResources=result.resources.filter(item=>!(item.path==='/workbench/v1/application'&&item.status===404)&&!(item.path==='/relay/v1/account'&&item.status===401));assert.deepEqual(unexpectedResources,[]);
  if(historyOnly){assert.ok(fixture.state.requests.filter(request=>request.path.startsWith('/workbench/v1/library/')&&request.path!=='/workbench/v1/library/projects').every(request=>!new URLSearchParams(request.query).has('projectScope')),'projectScope must be sent only to projects');assert.ok(fixture.state.requests.every(request=>request.method==='GET'),'History navigation must remain read-only');assert.equal(fixture.state.input.length,0);assert.equal(fixture.state.sockets,0);assert.equal(fixture.state.sse,0);}else assert.ok(fixture.state.sse>0);assert.ok(fixture.state.requests.filter(item=>/\/(workspace|live|history|run)(\/|$)/.test(item.path)).every(item=>item.path.startsWith(`/workbench/v1/runs/${runId}/`)));
 });
 const sourcesAfter=sourceManifest();result.sourceChangedDuringRun=Object.keys(sourcesBefore).filter(file=>sourcesBefore[file]!==sourcesAfter[file]);fs.writeFileSync(path.join(artifact.path,'source-manifest-after.json'),JSON.stringify(sourcesAfter,null,2)+'\n');if(result.sourceChangedDuringRun.length)result.failures.push({stage:'source-revision',message:'Production sources changed during the probe: '+result.sourceChangedDuringRun.join(', ')});
 success=result.failures.length===0;
}catch(error){result.failures.push({stage,message:String(error.message).slice(0,1600)});}
finally{
 if(browser)await browser.close().catch(()=>{});
 await fixture.close().catch(()=>{});if(server)await server.close().catch(()=>{});
 fs.rmSync(runtime,{recursive:true,force:true});result.status=success?'passed':'failed';result.synthetic={requests:fixture.state.requests,sockets:fixture.state.sockets,sse:fixture.state.sse,inputFrames:fixture.state.input.length,unexpected:fixture.state.unexpected};
 fs.writeFileSync(path.join(artifact.path,'result.json'),JSON.stringify(result,null,2)+'\n');artifact.finish(success);
 console.log(JSON.stringify({artifact:artifact.path,status:result.status,checks:result.checks,failures:result.failures,screenshots:result.screenshots.length},null,2));
 await close(success?0:1);
}
