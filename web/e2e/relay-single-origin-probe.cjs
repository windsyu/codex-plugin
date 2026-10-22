// Live synthetic relay + three real Applications. System Chrome only. Credentials
// arrive on stdin; never print URLs/fragments, cookies, or page/user content.
const fs = require('node:fs');
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const { expect } = require('playwright/test');
const { observeSecurePage, trackDocumentSockets } = require('./secure-probe-utils.cjs');
let stage = 'input', browser, server, reported = false, socketDiagnostics;
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
  assert.ok([2,3].includes(input.gateways.length));
  const otherIndex=input.gateways.length-1;
  for (const gateway of input.gateways) {
    const url=new URL(gateway.url); assert.equal(url.origin,origin.origin);
    assert.ok(uuid.test(gateway.gatewayId)); assert.equal(url.pathname,`/g/${gateway.gatewayId}/`);
    const auth=new URLSearchParams(url.hash.slice(1));
    assert.ok(uuid.test(auth.get('grant'))); assert.ok(uuid.test(auth.get('session'))); assert.equal(auth.get('api'),'3');
    assert.match(gateway.accountCookie,/^[^=;]+=[^;]+$/);
  }
  stage='chrome';
  server=await chromium.launchServer({ executablePath:process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',headless:true,host:'127.0.0.1',port:0,timeout:10000,handleSIGINT:false,handleSIGTERM:false,handleSIGHUP:false,args:['--no-proxy-server','--host-resolver-rules=MAP * 127.0.0.1, EXCLUDE localhost'] });
  browser=await chromium.connect(server.wsEndpoint());
  const alice=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  const bob=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  await alice.addCookies([cookie(input.gateways[0].accountCookie,origin)]);
  await bob.addCookies([cookie(input.gateways[otherIndex].accountCookie,origin)]);
  const pages=[]; let errors=0, assets=0, events=0, terminals=0, channels=0, starts=0, inputs=0, plaintext=0, markerLeaks=0, encryptedSearches=0;
  const marker='E2EE_SYNTHETIC_SEARCH_MARKER', markerBytes=Buffer.from(marker);
  const publicKeys=new Set();
  const activeSockets=new Map();
  const metric=value=>{if(value.kind==='input')inputs++;if(value.kind==='sse')events++;if(value.kind==='instrumentation_error')errors++;if(value.kind==='open'){if(value.start)starts++;if(value.terminal)terminals++;if(value.syntheticSearch)encryptedSearches++;}};
  async function observe(page,gateway,onSocket=()=>{}) {
    activeSockets.set(page,await trackDocumentSockets(page));
    await observeSecurePage(page,metric);
    page.on('pageerror',()=>errors++);
    page.on('request',request=>{const url=new URL(request.url());const path=url.pathname;if((path+url.search).includes(marker))markerLeaks++;if(path.startsWith(`/g/${gateway.gatewayId}/workbench/v1/`)&&!path.endsWith('/secure-channel'))plaintext++;});
    page.on('response',response=>{if(new URL(response.url()).pathname.startsWith('/assets/')&&response.status()===200)assets++;});
    page.on('websocket',ws=>{
      const url=new URL(ws.url());assert.equal(url.pathname,`/g/${gateway.gatewayId}/workbench/v1/secure-channel`);
      assert.ok(url.searchParams.has('relayGrant'));assert.equal(url.searchParams.get('relayApi'),'3');
      assert.ok(['application','run'].includes(url.searchParams.get('scope')));channels++;onSocket(url.searchParams.get('scope'));
      const inspectWire=frame=>{if(Buffer.from(frame.payload).includes(markerBytes))markerLeaks++;};
      ws.on('framereceived',inspectWire);
      ws.on('framesent',frame=>{inspectWire(frame);if(typeof frame.payload==='string'){const hello=JSON.parse(frame.payload);assert.equal(hello.type,'client_hello');assert.ok(!publicKeys.has(hello.publicKey),'fresh channel must use fresh ephemeral keys');publicKeys.add(hello.publicKey);}else assert.ok(Buffer.isBuffer(frame.payload));});
    });
  }
  for (const [index,context] of [[0,alice],[otherIndex,bob]]) {
    stage=`portal-${index}`; const portal=await context.newPage(); await portal.goto(origin.href);
    await expect(portal.locator('#signed-in')).toBeVisible();
    await expect(portal.locator('#workspace-access')).toBeVisible();
    assert.equal(new URL(portal.url()).pathname,'/computers');
    await expect(portal.locator('#gateways li')).toHaveCount(input.gateways.filter(gateway=>gateway.accountCookie===input.gateways[index].accountCookie).length);
    await expect(portal.locator('#gateways')).toContainText('在线'); await portal.close();
  }
  for (let index=0;index<input.gateways.length;index++) {
    stage=`workbench-${index}`; const gateway=input.gateways[index]; const context=gateway.accountCookie===input.gateways[0].accountCookie?alice:bob;
    const page=await context.newPage(); page.setDefaultTimeout(12000); pages.push(page);
    let pageSockets=0;
    await observe(page,gateway,()=>pageSockets++);
    stage=`workbench-${index}-document`;
    const response=await page.goto(gateway.url,{waitUntil:'domcontentloaded'}); assert.equal(response.status(),200);
    await expect(page.locator('.wb-home')).toBeVisible();
    stage=`workbench-${index}-api`;
    await page.waitForFunction(()=>window.__secureProbe.application);
    const result=await page.evaluate(()=>window.__secureProbe.application);
    assert.equal(result.instanceId,gateway.expectedInstanceId); assert.equal(result.remoteOwner,true);assert.equal(pageSockets,1);
    stage=`workbench-${index}-settings-public-relay-link-count`;
    await expect(page.getByRole('link',{name:'公网接入',exact:true})).toHaveCount(0);
    stage=`workbench-${index}-settings-click`;
    await page.getByRole('link',{name:'设置',exact:true}).click();
    stage=`workbench-${index}-settings-form-visible`;
    await expect(page.locator('#wb-settings-form')).toBeVisible();
    stage=`workbench-${index}-settings-route`;
    assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/settings`);
    stage=`workbench-${index}-settings-channel-reuse`;
    assert.equal(pageSockets,1,'Application SPA pages must reuse their channel');
    stage=`workbench-${index}-reload`;
    await page.reload(); await expect(page.locator('#wb-settings-form')).toBeVisible();
    await page.getByRole('link',{name:'全部历史',exact:true}).click();
    assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/history`);
    if (index === 0 && input.gateways.length === 3) {
      stage='remote-history-reader';
      await page.locator('.wb-library-entry').filter({hasText:'Synthetic remote history conversation'}).click();
      await expect(page.locator('.wb-library-body-scroll')).toContainText('Synthetic remote history conversation');
      assert.equal(new URL(page.url()).search,'','history reading selectors must stay in the fragment');
      await page.getByRole('button',{name:'调整项目归属',exact:true}).click();
      const editor=page.getByRole('dialog',{name:'调整项目归属'});
      await expect(editor).toContainText('用户调整已生效');
      await editor.getByRole('button',{name:'关闭',exact:true}).click();
      await page.getByRole('button',{name:'← 返回会话列表',exact:true}).click();
      await page.goBack();
      await page.goBack();
    }
    await page.goBack(); await expect(page.locator('#wb-settings-form')).toBeVisible();
    await page.getByRole('link',{name:'项目概览',exact:true}).click();
    const dimensions=await page.evaluate(()=>({viewport:document.documentElement.clientWidth,content:document.documentElement.scrollWidth}));
    assert.ok(dimensions.content<=dimensions.viewport+1,'mobile document must not overflow horizontally');
    if(index===0) {
      stage='encrypted-search-privacy';
      await page.getByRole('link',{name:'全部历史',exact:true}).click();
      const search=page.getByRole('searchbox',{name:'搜索会话和消息',exact:true});
      await search.fill(marker);await search.press('Enter');
      await page.waitForFunction(value=>new URLSearchParams(location.hash.slice(1)).get('q')===value,marker);
      assert.equal(new URL(page.url()).search,'');
      await expect.poll(()=>encryptedSearches).toBeGreaterThan(0);
      await page.reload();await expect(search).toHaveValue(marker);
      assert.equal(new URL(page.url()).search,'');assert.equal(new URLSearchParams(new URL(page.url()).hash.slice(1)).get('q'),marker);
      await search.fill('');await search.press('Enter');
      await page.getByRole('link',{name:'项目概览',exact:true}).click();
    }
    if(gateway.runId) {
      stage='run-navigation'; const run=new URL(gateway.url); run.searchParams.set('run',gateway.runId);const beforeRun=pageSockets;
      const response=await page.goto(run.href); assert.equal(response.status(),200);
      await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready','true',{timeout:15000});
      assert.equal(new URL(page.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/chat`);
      assert.ok(events>0); assert.ok(terminals>0);
      assert.equal(pageSockets,beforeRun+2,'Run document opens one Application and one multiplexed Run channel');
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
      const tab=await context.newPage();let tabSockets=0;await observe(tab,gateway,()=>tabSockets++);await tab.goto(run.href); await expect(tab.locator('.wb-terminal')).toHaveAttribute('data-ready','true');
      assert.equal(tabSockets,2,'new tab owns fresh Application and Run channels');
      assert.equal(new URL(tab.url()).pathname,`/g/${gateway.gatewayId}/runs/${gateway.runId}/chat`);await tab.close();
    }
  }
  stage='pinned-context-isolation';
  const grant=new URLSearchParams(new URL(input.gateways[0].url).hash.slice(1)).get('grant');
  const wrong=await pages[1].evaluate(async ({gatewayId,grant})=>(await fetch(`/relay/v1/gateways/${gatewayId}/access`,{headers:{'X-Workbench-Grant':grant,'X-Workbench-Api-Version':'3'}})).status,{gatewayId:input.gateways[1].gatewayId,grant});
  assert.equal(wrong,401);
  await alice.addCookies([cookie(input.gateways[otherIndex].accountCookie,origin)]);
  const switched=await pages[0].evaluate(async ({gatewayId,grant})=>(await fetch(`/relay/v1/gateways/${gatewayId}/access`,{headers:{'X-Workbench-Grant':grant,'X-Workbench-Api-Version':'3'}})).status,{gatewayId:input.gateways[0].gatewayId,grant});
  assert.equal(switched,401);
  stage='parent-revocation-restore-cookie';
  const beforeRevocation=[];
  for(let index=0;index<pages.length;index++) {
    stage=`parent-revocation-page-${index}-tracker-agrees-with-native`;
    await expect.poll(async()=>activeSockets.get(pages[index]).size===(await pages[index].evaluate(()=>window.__secureProbe.socketCounts)).live).toBe(true);
    beforeRevocation.push({page:index,tracked:activeSockets.get(pages[index]).size,native:await pages[index].evaluate(()=>window.__secureProbe.socketCounts)});
  }
  socketDiagnostics=async()=>({before:beforeRevocation,after:await Promise.all(pages.map(async(page,index)=>({page:index,tracked:activeSockets.get(page).size,native:await page.evaluate(()=>window.__secureProbe.socketCounts).catch(()=>null)})))});
  // The Rust fixture supplies dedicated probe parents, so this logout cannot
  // invalidate the independent parent used by the remaining Rust assertions.
  await alice.addCookies([cookie(input.gateways[0].accountCookie,origin)]);
  stage='parent-revocation-logout-request';
  const logout=await pages[0].evaluate(async ()=>(await fetch('/relay/v1/logout',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'})).status);
  stage='parent-revocation-logout-status';
  assert.ok(logout>=200&&logout<300);
  for(let index=0;index<pages.length;index++)if(input.gateways[index].accountCookie===input.gateways[0].accountCookie){
    stage=`parent-revocation-alice-${index}-auth-message`;
    await expect(pages[index].locator('.wb-unavailable')).toContainText('登录或工作台授权已失效');
    stage=`parent-revocation-alice-${index}-business-dom-cleared`;
    await expect(pages[index].locator('.wb-home, .wb-shell, .wb-terminal, .wb-library-body-scroll')).toHaveCount(0);
    stage=`parent-revocation-alice-${index}-native-sockets-closed`;
    await expect.poll(()=>pages[index].evaluate(()=>window.__secureProbe.socketCounts.live)).toBe(0);
    stage=`parent-revocation-alice-${index}-sockets-closed`;
    await expect.poll(()=>activeSockets.get(pages[index]).size).toBe(0);
  }
  const bobPage=pages[otherIndex];
  stage='parent-revocation-bob-home';
  await expect(bobPage.locator('.wb-home')).toBeVisible();
  stage='parent-revocation-bob-settings-click';
  await bobPage.getByRole('link',{name:'设置',exact:true}).click();
  stage='parent-revocation-bob-settings-form-visible';
  await expect(bobPage.locator('#wb-settings-form')).toBeVisible();
  stage='parent-revocation-bob-socket-retained';
  assert.ok(activeSockets.get(bobPage).size>0);
  await expect.poll(()=>bobPage.evaluate(()=>window.__secureProbe.socketCounts.open)).toBeGreaterThan(0);
  await expect.poll(async()=>activeSockets.get(bobPage).size===(await bobPage.evaluate(()=>window.__secureProbe.socketCounts)).live).toBe(true);
  stage='final-metrics-barrier';
  await Promise.all(pages.map(page=>page.evaluate(()=>window.__secureProbeMetric({kind:'barrier'}))));
  stage='final-metrics-assertions';
  assert.equal(plaintext,0);assert.equal(markerLeaks,0);assert.ok(encryptedSearches>0);assert.equal(errors,0); assert.ok(assets>=input.gateways.length);assert.equal(starts,0);assert.equal(inputs,0);
  report({stage:'passed',chrome:browser.version(),mobileWidth:390,portals:2,workbenches:input.gateways.length,assetResponses:assets,sse:events,terminalStreams:terminals,encryptedChannels:channels,crossGatewayDenied:true,accountSwitchDenied:true,parentRevocationClearsPages:true,otherAccountPreserved:true,deepRoutes:true,refreshAndBack:true,sameRunTerminalPreserved:true,implicitLaunches:starts,implicitInputs:inputs,plaintextBusinessRequests:plaintext,searchPrivacy:true});
  await alice.close(); await bob.close(); await shutdown(0);
})().catch(async error=>{
  const message=typeof error?.message==='string'?error.message:'';
  const reason=/strict mode violation/.test(message)?'ambiguous_locator':/toBeVisible/.test(message)?'expected_element_not_visible':/toHaveCount/.test(message)?'unexpected_element_count':/Target.*closed|page.*closed|browser.*closed/.test(message)?'target_closed':/detached/.test(message)?'element_detached':/Timeout|timed out/.test(message)?'operation_timeout':/Execution context was destroyed/.test(message)?'navigation_context_changed':'unclassified';
  const sockets=socketDiagnostics?await socketDiagnostics():undefined;
  report({stage:'failed',check:stage,kind:['AssertionError','TimeoutError'].includes(error?.name)?error.name:'operation_failed',reason,sockets});void shutdown(1);
});
