// Live synthetic relay + three real Applications. System Chrome only. Credentials
// arrive on stdin; never print URLs/fragments, cookies, or page/user content.
const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const { expect } = require('playwright/test');
let stage = 'input', browser, server, reported = false;
const report = value => { if (!reported) { process.stdout.write(JSON.stringify(value) + '\n'); reported = true; } };
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
function cookie(value, origin) {
  const at = value.indexOf('='); const url = new URL(origin); url.protocol = 'https:';
  return { name:value.slice(0,at), value:value.slice(at+1), url:url.href, secure:true, httpOnly:true, sameSite:'Strict' };
}
async function shutdown(code) {
  if (server) await Promise.race([server.close().catch(() => {}),new Promise(resolve => setTimeout(resolve,3000))]);
  if (server) await server.kill().catch(() => {});
  process.exit(code);
}
for (const signal of ['SIGTERM','SIGINT','SIGHUP']) process.on(signal,() => { report({stage:'failed',check:stage}); void shutdown(1); });
setTimeout(() => { report({stage:'failed',check:stage,reason:'deadline'}); void shutdown(1); },85000).unref();

(async () => {
  const raw=fs.readFileSync(0,'utf8'); assert.ok(raw.length<65536);
  const input=JSON.parse(raw); const origin=new URL(input.portalOrigin);
  assert.equal(origin.protocol,'http:'); assert.ok(['app.localhost','127.0.0.1'].includes(origin.hostname));
  assert.equal(input.gateways.length,3);
  for (const gateway of input.gateways) {
    const url=new URL(gateway.url); assert.equal(url.origin,origin.origin);
    assert.ok(uuid.test(gateway.gatewayId)); assert.equal(url.pathname,`/g/${gateway.gatewayId}/`);
    const auth=new URLSearchParams(url.hash.slice(1));
    assert.ok(uuid.test(auth.get('grant'))); assert.ok(uuid.test(auth.get('session'))); assert.equal(auth.get('api'),'1');
    assert.match(gateway.accountCookie,/^[^=;]+=[^;]+$/);
  }
  stage='chrome';
  server=await chromium.launchServer({ executablePath:process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true,host:'127.0.0.1',port:0,timeout:10000,handleSIGINT:false,handleSIGTERM:false,handleSIGHUP:false,args:['--no-proxy-server','--host-resolver-rules=MAP * 127.0.0.1, EXCLUDE localhost'] });
  browser=await chromium.connect(server.wsEndpoint());
  const alice=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  const bob=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  await alice.addCookies([cookie(input.gateways[0].accountCookie,origin)]);
  await bob.addCookies([cookie(input.gateways[2].accountCookie,origin)]);
  const pages=[]; let errors=0, assets=0, events=0, terminals=0, starts=0, inputs=0;
  for (const [index,context] of [[0,alice],[2,bob]]) {
    stage=`portal-${index}`; const portal=await context.newPage(); await portal.goto(origin.href);
    await expect(portal.locator('#signed-in')).toBeVisible();
    await expect(portal.locator('#workspace-access')).toBeVisible();
    assert.equal(new URL(portal.url()).pathname,'/computers');
    await expect(portal.locator('#gateways li')).toHaveCount(index===0?2:1);
    await expect(portal.locator('#gateways')).toContainText('在线'); await portal.close();
  }
  for (let index=0;index<3;index++) {
    stage=`workbench-${index}`; const gateway=input.gateways[index]; const context=index<2?alice:bob;
    const page=await context.newPage(); page.setDefaultTimeout(12000); pages.push(page);
    page.on('pageerror',()=>errors++);
    page.on('request',request=>{if(request.method()==='POST'&&new URL(request.url()).pathname.endsWith('/application/launch'))starts++;});
    page.on('response',response=>{const url=new URL(response.url()); if(url.pathname.startsWith('/assets/') && response.status()===200) assets++; if(url.pathname.endsWith('/live/events')&&response.status()===200)events++;});
    let pageSockets=0;
    page.on('websocket',ws=>{const url=new URL(ws.url()); assert.ok(url.pathname.startsWith(`/g/${gateway.gatewayId}/workbench/v1/`)); assert.ok(url.searchParams.has('relayGrant')); terminals++;pageSockets++;ws.on('framesent',frame=>{try{if(JSON.parse(frame.payload).command?.type==='input')inputs++;}catch{}});});
    stage=`workbench-${index}-document`;
    const response=await page.goto(gateway.url,{waitUntil:'domcontentloaded'}); assert.equal(response.status(),200);
    await expect(page.locator('.wb-home')).toBeVisible();
    stage=`workbench-${index}-api`;
    const result=await page.evaluate(async ({gatewayId,url})=>{
      // The page intentionally consumes/removes the fragment on bootstrap.
      const auth=new URLSearchParams(new URL(url).hash.slice(1));
      const response=await fetch(`/g/${gatewayId}/workbench/v1/application`,{headers:{'X-Workbench-Grant':auth.get('grant'),'X-Workbench-Api-Version':'1'}});
      return {status:response.status,body:await response.json()};
    },gateway);
    assert.equal(result.status,200); assert.equal(result.body.instanceId,gateway.expectedInstanceId); assert.equal(result.body.remoteOwner,true);
    stage=`workbench-${index}-settings`;
    await expect(page.getByRole('link',{name:'公网接入',exact:true})).toHaveCount(0);
    await page.getByRole('link',{name:'设置',exact:true}).click(); await expect(page.locator('#wb-settings-form')).toBeVisible();
    assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/settings`);
    stage=`workbench-${index}-reload`;
    await page.reload(); await expect(page.locator('#wb-settings-form')).toBeVisible();
    await page.getByRole('link',{name:'全部历史',exact:true}).click();
    assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/history`);
    await page.goBack(); await expect(page.locator('#wb-settings-form')).toBeVisible();
    await page.getByRole('link',{name:'项目概览',exact:true}).click();
    const dimensions=await page.evaluate(()=>({viewport:document.documentElement.clientWidth,content:document.documentElement.scrollWidth}));
    assert.ok(dimensions.content<=dimensions.viewport+1,'mobile document must not overflow horizontally');
    if(gateway.runId) {
      stage='run-navigation'; const run=new URL(gateway.url); run.searchParams.set('run',gateway.runId);
      const response=await page.goto(run.href); assert.equal(response.status(),200);
      await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true',{timeout:15000});
      assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/chat`);
      assert.ok(events>0); assert.ok(terminals>0);
      const terminal=await page.locator('.wb-terminal').elementHandle();const sockets=pageSockets;
      await page.getByRole('link',{name:'文件',exact:true}).click();
      assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/files`);
      await page.getByRole('link',{name:'用量与状态',exact:true}).click();
      await page.goBack();
      assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/files`);
      await page.getByRole('link',{name:'实时对话',exact:true}).click();
      assert.ok(await terminal.evaluate(node=>node===document.querySelector('.wb-terminal')),'same Run keeps its terminal DOM');
      assert.equal(pageSockets,sockets,'feature navigation must not reopen terminal WebSocket');
      await page.reload();await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true');
      const tab=await context.newPage(); await tab.goto(run.href); await expect(tab.locator('.wb-terminal')).toBeVisible();
      assert.equal(new URL(tab.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/chat`);await tab.close();
    }
  }
  stage='pinned-context-isolation';
  const grant=new URLSearchParams(new URL(input.gateways[0].url).hash.slice(1)).get('grant');
  const wrong=await pages[1].evaluate(async ({gatewayId,grant})=>(await fetch(`/g/${gatewayId}/workbench/v1/application`,{headers:{'X-Workbench-Grant':grant,'X-Workbench-Api-Version':'1'}})).status,{gatewayId:input.gateways[1].gatewayId,grant});
  assert.equal(wrong,401);
  await alice.addCookies([cookie(input.gateways[2].accountCookie,origin)]);
  const switched=await pages[0].evaluate(async ({gatewayId,grant})=>(await fetch(`/g/${gatewayId}/workbench/v1/application`,{headers:{'X-Workbench-Grant':grant,'X-Workbench-Api-Version':'1'}})).status,{gatewayId:input.gateways[0].gatewayId,grant});
  assert.equal(switched,401); assert.equal(errors,0); assert.ok(assets>=3);assert.equal(starts,0);assert.equal(inputs,0);
  report({stage:'passed',chrome:browser.version(),mobileWidth:390,portals:2,workbenches:3,assetResponses:assets,sse:events,terminalSockets:terminals,crossGatewayDenied:true,accountSwitchDenied:true,deepRoutes:true,refreshAndBack:true,sameRunTerminalPreserved:true,implicitLaunches:starts,implicitInputs:inputs});
  await alice.close(); await bob.close(); await shutdown(0);
})().catch(error=>{report({stage:'failed',check:stage,kind:['AssertionError','TimeoutError'].includes(error?.name)?error.name:'operation_failed'});void shutdown(1);});
