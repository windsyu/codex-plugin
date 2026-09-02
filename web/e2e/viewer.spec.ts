import { expect, test, type Page } from '@playwright/test';

const longId = `thread-${'x'.repeat(180)}`;

function envelope(data: unknown) {
  return { apiVersion: 'v1', asOfEventSeq: 8, data };
}

function thread(threadKey: string, name: string) {
  return {
    threadKey, codexThreadId: threadKey, storeSourceId: 'source-1', name, archived: false, status: 'active', stale: false,
    captureCompleteness: 'durable_partial', completenessReasons: ['durable_turn_not_terminal'], lastEventSeq: 8, ruleVersion: 'v1',
    project: { key: 'project', name: 'Fixture project', path: `/very/${'long'.repeat(50)}/project` },
    context: { session: { modelProvider: 'openai' }, runtime: { cwd: `/very/${'long'.repeat(50)}/cwd`, model: 'fixture' } }
  };
}

async function mockApi(page: Page, delays: Record<string, number> = {}) {
  let rawRequests = 0;
  await page.addInitScript(() => sessionStorage.setItem('observer-token', 'fixture-token'));
  await page.route('**/v1/**', async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    let data: unknown;
    if (path === '/v1/health') data = { status: 'healthy', ready: true, control:{enabled:true,tailscaleMutationAccess:false}, privacy: { legacyRedactionEvents: 1, warning: 'legacy warning' } };
    else if (path === '/v1/projects') data = [{ project: { key: 'project', name: 'Fixture project', path: '/fixture' }, threadCount: 2, currentThreadCount: 2, lastRecencyAtMs: 8 }];
    else if (path === '/v1/sources') data = [{ sourceId: 'source-1', kind: 'rollout', stableIdentity: 'fixture', status: 'ready', currentEpoch: { decodeErrorCount: 1, unknownEventCount: 1 } }];
    else if (path === '/v1/threads') data = [thread(longId, 'Slow thread'), thread('thread-fast', 'Fast thread'), {
      ...thread('thread-recent', 'Recent conversation'), project: undefined,
      context: { session: { originator: 'Codex Desktop' }, runtime: { cwd: '/Users/demo/Documents/Codex/2026-08-28/generated-name', model: 'fixture' } }
    }];
    else if (path === '/v1/search') data = [{ entityKey: 'entity', threadKey: 'thread-fast', turnId: 'turn-fast', itemId: 'item-fast', snippet: 'safe matching snippet', score: 1, lastEventSeq: 8 }];
    else if (path.endsWith('/turns')) data = [{ turnId: path.includes(encodeURIComponent(longId)) ? 'turn-slow' : 'turn-fast', status: 'completed', captureCompleteness: 'durable_partial', completenessReasons: ['terminal missing'], coverage: {}, startedAtMs: 1, completedAtMs: 2, raw: {}, lastEventSeq: 8 }];
    else if (path.endsWith('/items')) {
      const turnId = path.includes('thread-fast') ? 'turn-fast' : 'turn-slow';
      data = [
        { turnScope: turnId, turnId, itemId: `user-${turnId}`, itemType: 'user_message', status: 'completed', summaryText: 'fixture question', raw: {}, provenance: {}, lastEventSeq: 6 },
        { turnScope: turnId, turnId, itemId: path.includes('thread-fast') ? 'item-fast' : 'item-slow', itemType: 'command_execution', status: 'completed', summaryText: 'command output', raw: { payload: { command: 'echo fixture', phase: 'completed' } }, provenance: {}, lastEventSeq: 7 },
        { turnScope: turnId, turnId, itemId: `answer-${turnId}`, itemType: 'agent_message', status: 'completed', summaryText: 'fixture answer', raw: {}, provenance: {}, lastEventSeq: 8 }
      ];
    }
    else if (path.endsWith('/events')) { rawRequests += 1; data = [{ eventSeq: 8, eventId: 'event-8', sourceId: 'source-1', sourceEpoch: 'epoch-1', sourceSeq: 8, observedAtMs: 8, threadKey: 'thread-fast', codexThreadId: 'thread-fast', method: 'event/task_complete', phase: 'completed', durability: 'durable', raw: { text: '<svg onload=alert(1)>' }, redaction: { rule: 'v2' }, decodeStatus: 'decoded', storedRawHash: 'hash' }]; }
    else if (path.startsWith('/v1/threads/')) {
      const key = decodeURIComponent(path.split('/').at(-1)!);
      if (delays[key]) await new Promise((resolve) => setTimeout(resolve, delays[key]));
      data = { thread: thread(key, key === 'thread-fast' ? 'Fast thread' : 'Slow thread'), sources: [], coverageSummary: { durable_partial: 1 }, pendingRequests: [], projectionConflicts: [], relations: { parent: { threadKey: `${'parent'.repeat(50)}`, resolved: false }, children: [] }, diagnostics: { decodeErrors: 1, unknownVariants: 1, conflicts: 0 } };
    } else data = [];
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(envelope(data)) });
  });
  return () => rawRequests;
}

async function mockControlApi(page: Page, activeTurn = true) {
  await mockApi(page);
  const capture = { uploadedBytes: 0, input: undefined as Record<string, unknown> | undefined,
    action: undefined as Record<string, unknown> | undefined, createdThread: undefined as Record<string, unknown> | undefined,
    setting: undefined as Record<string, unknown> | undefined, streamPaths: [] as string[], projectionReady:false,
    threadDetailRequests:0 };
  await page.route('**/v1/threads**', async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/v1/threads' && capture.createdThread) {
      const data = [thread('created-thread', 'Cleared thread'), thread(longId, 'Slow thread'), thread('thread-fast', 'Fast thread')];
      await route.fulfill({ status:200,contentType:'application/json',body:JSON.stringify(envelope(data)) }); return;
    }
    if (path === '/v1/threads/created-thread') {
      const data = {thread:thread('created-thread','Cleared thread'),sources:[],coverageSummary:{},pendingRequests:[],projectionConflicts:[],
        relations:{children:[]},diagnostics:{decodeErrors:0,unknownVariants:0,conflicts:0}};
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(envelope(data))}); return;
    }
    if (path === '/v1/threads/created-thread/turns' || path === '/v1/threads/created-thread/items') {
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(envelope([]))}); return;
    }
    await route.fallback();
  });
  await page.route('**/v1/threads/thread-fast/items**', async (route) => {
    const data: Record<string, unknown>[] = [
      { turnScope:'turn-fast',turnId:'turn-fast',itemId:'user-turn-fast',itemType:'user_message',status:'completed',summaryText:'fixture question',raw:{},provenance:{},lastEventSeq:6 },
      { turnScope:'turn-fast',turnId:'turn-fast',itemId:'item-fast',itemType:'command_execution',status:'completed',summaryText:'command output',raw:{payload:{command:'echo fixture',phase:'completed'}},provenance:{},lastEventSeq:7 },
      { turnScope:'turn-fast',turnId:'turn-fast',itemId:'answer-turn-fast',itemType:'agent_message',status:'completed',summaryText:'fixture answer',raw:{},provenance:{},lastEventSeq:8 }
    ];
    const clientId = capture.input?.clientUserMessageId;
    if (capture.projectionReady && typeof clientId === 'string') data.push({ turnScope:'turn-active',turnId:'turn-active',itemId:'projected-user',
      itemType:'user_message',status:'completed',summaryText:String(capture.input?.text || 'image message'),
      raw:{payload:{clientUserMessageId:clientId}},provenance:{},lastEventSeq:9 });
    await route.fulfill({ status:200,contentType:'application/json',body:JSON.stringify(envelope(data)) });
  });
  await page.route('**/v1/threads/thread-fast', async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname !== '/v1/threads/thread-fast') return route.fallback();
    capture.threadDetailRequests += 1;
    const data = { thread: thread('thread-fast', 'Fast thread'), sources: [], coverageSummary: { durable_partial: 1 }, projectionConflicts: [],
      relations: { children: [] }, diagnostics: { decodeErrors: 0, unknownVariants: 0, conflicts: 0 }, pendingRequests: [{
        requestKey: 'signed-request', sourceId: 'source-1', sourceEpoch: 'epoch-1', requestId: 'approval-1', requestType: 'approval',
        state: 'pending', requestVersion: 1, requestEventSeq: 8, payload: { command: 'cargo test', cwd: '/fixture/project' }
      }] };
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(envelope(data)) });
  });
  await page.route('**/v2/**', async (route) => {
    const request = route.request(); const url = new URL(request.url());
    expect(url.searchParams.has('token')).toBe(false);
    expect(request.headers().authorization).toBe('Bearer fixture-token');
    if (url.pathname === '/v2/stream') {
      capture.streamPaths.push(`${url.pathname}${url.search}`);
      if (capture.input?.clientUserMessageId) capture.projectionReady = true;
      await route.fulfill({ status: 200, contentType: 'text/event-stream', body: 'event: observer\nid: signed-stream-cursor\ndata: {"eventSeq":9}\n\n' });
    } else if (url.pathname === '/v2/control/sources') {
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:[{sourceId:'source-1',sourceEpoch:'epoch-1',supervisorVersion:1,state:'ready'}] }) });
    } else if (url.pathname === '/v2/control/catalog') {
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{
        sourceId:'source-1',sourceEpoch:'epoch-1',threadLoaded:true,activeTurnId:activeTurn ? 'turn-active' : undefined,collaborationMode:{mode:'default'},
        capabilities:{entries:{'model/list':{available:true,experimental:false,data:{data:[{id:'fixture',displayName:'Fixture',supportsPersonality:true,
          supportedReasoningEfforts:[{reasoningEffort:'low',label:'Low'},{reasoningEffort:'high',label:'High'}]}]}},
          'permissionProfile/list':{available:true,experimental:false,data:{data:[{id:'workspace',displayName:'Workspace'}]}}}},
        slashCommands:[{name:'/status',capability:'status',interactionRequiredWithoutArgument:false},
          ...(activeTurn ? [{name:'/interrupt',capability:'turn.interrupt',interactionRequiredWithoutArgument:false}]
            : [{name:'/clear',capability:'thread.clear',interactionRequiredWithoutArgument:false}]),
          {name:'/plan',capability:'thread.plan',interactionRequiredWithoutArgument:false}]
      } }) });
    } else if (url.pathname === '/v2/uploads/images') {
      capture.uploadedBytes = request.postDataBuffer()?.byteLength || 0;
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{uploadId:'upload-1',mimeType:'image/png',sizeBytes:capture.uploadedBytes,expiresAtMs:Date.now()+60_000} }) });
    } else if (url.pathname === '/v2/threads/thread-fast/inputs') {
      capture.input = request.postDataJSON();
      const state = capture.input?.text === 'simulate uncertainty' ? 'outcome_unknown' : 'dispatching';
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{commandId:'command-1',state} }) });
    } else if (url.pathname === '/v2/threads') {
      capture.createdThread = request.postDataJSON();
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{commandId:'thread-command',state:'completed',result:{threadId:'created-thread'}} }) });
    } else if (url.pathname === '/v2/commands') {
      if (request.method() === 'GET') {
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:[] }) });
      } else {
        capture.setting = request.postDataJSON();
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{commandId:'setting-command',state:'completed'} }) });
      }
    } else if (url.pathname === '/v2/requests/signed-request/actions') {
      capture.action = request.postDataJSON();
      await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ apiVersion:'v2',data:{commandId:'command-2',state:'completed'} }) });
    } else {
      await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error:{message:'not mocked'} }) });
    }
  });
  return capture;
}

async function mockSessionKernel(
  page: Page,
  inputLeaseConflict = false,
  configuredMode: 'preview' | 'tui' = 'preview',
  activeTurn = false
) {
  await mockApi(page);
  await page.route('**/v2/control/sources', async (route) => {
    await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({
      apiVersion:'v2',data:[{sourceId:'source-1',sourceEpoch:'epoch-1',supervisorVersion:1,state:'ready'}]
    })});
  });
  await page.route('**/v2/control/catalog**', async (route) => {
    await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({
      apiVersion:'v2',data:{sourceId:'source-1',sourceEpoch:'epoch-1',threadLoaded:true,
        capabilities:{entries:{}},slashCommands:[]}
    })});
  });
  await page.route('**/v1/health', async (route) => {
    await route.fulfill({ status:200,contentType:'application/json',body:JSON.stringify(envelope({
      status:'healthy',ready:true,control:{enabled:true,tailscaleMutationAccess:false},
      sessionKernel:{configuredMode,compiled:true,workerAvailable:true,fakeCliAvailable:true,cliAvailable:true}
    })) });
  });
  const worker = { workerId:'fake-worker',state:'ready',pid:123,cwd:'/fixture/project',rows:24,cols:80,outputSeq:1,
    terminalRetainedBytes:16,terminalCheckpointBytes:15,ptyEof:false,errorCode:null,
    inputLease:{leaseId:null,ownerAttachmentId:null,state:'none',version:0},
    persistedWorker:{workerId:'fake-worker',sourceId:'source-1',sourceEpoch:'epoch-1',state:'ready',version:4,
      inputLeaseVersion:0,primaryThreadId:'thread-fast'},
    activeTurns:activeTurn ? [{codexThreadId:'thread-fast',codexTurnId:'turn-active'}] : [] };
  let attachCount = 0;
  let resumeControlValidated = false;
  const createBodies: Record<string, unknown>[] = [];
  let interruptBody: Record<string, unknown> | undefined;
  let stopBody: Record<string, unknown> | undefined;
  const attachmentToken = 'b'.repeat(64);
  await page.route('**/v2/sessions**', async (route) => {
    const request = route.request(); const url = new URL(request.url());
    if (url.pathname === '/v2/sessions') {
      const body = request.postDataJSON() as Record<string, unknown>;
      createBodies.push(body);
      expect(body).toMatchObject({ sourceId:'source-1',sourceEpoch:'epoch-1',expectedSupervisorVersion:1 });
      await route.fulfill({status:201,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:worker})});
    } else if (url.pathname === '/v2/sessions/fake-worker/events' && request.method() === 'GET') {
      await route.fulfill({status:200,contentType:'text/event-stream',body:`event: session_state\nid: ${worker.outputSeq}\ndata: ${JSON.stringify(worker)}\n\n`});
    } else if (url.pathname === '/v2/sessions/fake-worker' && request.method() === 'GET') {
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:worker})});
    } else if (url.pathname.endsWith('/attach')) {
      const body = request.postDataJSON() as { resumeAttachmentId?: string | null; resumeAttachmentToken?: string | null };
      if (attachCount > 0) {
        resumeControlValidated = body.resumeAttachmentId === 'attachment-1' && body.resumeAttachmentToken === attachmentToken;
      }
      attachCount += 1;
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:{
        attachmentId:'attachment-1',attachmentToken,descriptor:`${'a'.repeat(60)}${attachCount}`,descriptorExpiresInSeconds:30,inputLeaseVersion:0
      }})});
    } else if (url.pathname.endsWith('/input-lease')) {
      const body = request.postDataJSON() as { attachmentId?: string; attachmentToken?: string };
      if (body.attachmentId !== 'attachment-1' || body.attachmentToken !== attachmentToken) {
        await route.fulfill({status:401,contentType:'application/json',body:JSON.stringify({error:{code:'ATTACHMENT_TOKEN_INVALID',message:'invalid token'}})});
        return;
      }
      if (inputLeaseConflict) {
        worker.inputLease = {leaseId:'other-lease',ownerAttachmentId:'other-attachment',state:'active',version:1};
        await route.fulfill({status:409,contentType:'application/json',body:JSON.stringify({error:{code:'INPUT_LEASE_CONFLICT',message:'another attachment owns terminal input'}})});
        return;
      }
      worker.inputLease = {leaseId:'lease-1',ownerAttachmentId:'attachment-1',state:'active',version:1};
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:worker.inputLease})});
    } else if (url.pathname.endsWith('/stop')) {
      stopBody = request.postDataJSON() as Record<string, unknown>;
      worker.state = 'stopping';
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:worker})});
    } else if (url.pathname.endsWith('/interrupt')) {
      interruptBody = request.postDataJSON() as Record<string, unknown>;
      worker.activeTurns = [];
      await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({apiVersion:'v2',data:{
        commandId:'interrupt-command',state:'completed'
      }})});
    } else {
      await route.fulfill({status:404,contentType:'application/json',body:JSON.stringify({error:{message:'not mocked'}})});
    }
  });
  await page.addInitScript(() => {
    const target = window as unknown as {
      WebSocket: typeof WebSocket;
      terminalFrames: string[];
      disconnectTerminal?: () => void;
      pushTerminalOutput?: (text: string, outputSeq: number) => void;
    };
    target.terminalFrames = [];
    class FixtureWebSocket {
      static readonly CONNECTING = 0; static readonly OPEN = 1; static readonly CLOSING = 2; static readonly CLOSED = 3;
      readonly CONNECTING = 0; readonly OPEN = 1; readonly CLOSING = 2; readonly CLOSED = 3;
      readyState = 0; binaryType: BinaryType = 'blob'; bufferedAmount = 0; extensions = ''; protocol = 'codex-terminal-v1';
      onopen: ((event: Event) => void) | null = null; onmessage: ((event: MessageEvent) => void) | null = null;
      onerror: ((event: Event) => void) | null = null; onclose: ((event: CloseEvent) => void) | null = null;
      constructor(public readonly url: string, public readonly protocols?: string | string[]) {
        target.disconnectTerminal = () => {
          if (this.readyState !== FixtureWebSocket.OPEN) return;
          this.readyState = FixtureWebSocket.CLOSED;
          this.onclose?.(new CloseEvent('close',{code:1006,reason:'fixture disconnect',wasClean:false}));
        };
        target.pushTerminalOutput = (text, outputSeq) => {
          const bytes = new TextEncoder().encode(text); const output = new Uint8Array(9 + bytes.length);
          output[0] = 1; new DataView(output.buffer).setBigUint64(1, BigInt(outputSeq), false); output.set(bytes, 9);
          this.onmessage?.(new MessageEvent('message',{data:output.buffer}));
        };
        window.setTimeout(() => {
          this.readyState = 1; this.onopen?.(new Event('open'));
          this.onmessage?.(new MessageEvent('message',{data:JSON.stringify({type:'snapshot',checkpointSeq:1,fromSeq:2,toSeq:1,
            rows:24,cols:80,screen:btoa('SESSION_READY\r\n'),replay:'',encoding:'base64',complete:true,truncated:false})}));
          this.onmessage?.(new MessageEvent('message',{data:JSON.stringify({type:'state',worker:{workerId:'fake-worker',state:'ready',pid:123,
            cwd:'/fixture/project',rows:24,cols:80,outputSeq:1,terminalRetainedBytes:16,terminalCheckpointBytes:15,ptyEof:false,
            inputLease:{leaseId:null,ownerAttachmentId:null,state:'none',version:0}}})}));
        }, 10);
      }
      send(data: string | ArrayBufferLike | Blob | ArrayBufferView) {
        if (typeof data !== 'string') return;
        target.terminalFrames.push(data);
        const frame = JSON.parse(data);
        if (frame.type === 'input') {
          const bytes = new TextEncoder().encode(`ECHO:${frame.data}`); const output = new Uint8Array(9 + bytes.length);
          output[0] = 1; new DataView(output.buffer).setBigUint64(1, 2n, false); output.set(bytes, 9);
          window.setTimeout(() => this.onmessage?.(new MessageEvent('message',{data:output.buffer})), 0);
        }
      }
      close(code = 1000, reason = '') { this.readyState = 3; this.onclose?.(new CloseEvent('close',{code,reason,wasClean:true})); }
      addEventListener() {} removeEventListener() {} dispatchEvent() { return true; }
    }
    target.WebSocket = FixtureWebSocket as unknown as typeof WebSocket;
  });
  return {
    getAttachCount: () => attachCount,
    getResumeControlValidated: () => resumeControlValidated,
    getCreateBodies: () => createBodies,
    getInterruptBody: () => interruptBody,
    getStopBody: () => stopBody
  };
}

for (const width of [390, 820, 1280, 1440]) {
  test(`bounds layout at ${width}px and switches mobile navigation`, async ({ page }) => {
    await mockApi(page);
    await page.setViewportSize({ width, height: 900 });
    await page.goto('/');
    await expect(page.getByText('Slow thread').first()).toBeVisible();
    await page.getByText('Slow thread').first().click();
    await expect(page.getByRole('heading', { name: 'Slow thread' })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
    if (width <= 820) {
      await expect(page.locator('.sidebar')).toBeHidden();
      await page.getByRole('button', { name: /返回列表/ }).click();
      await expect(page.locator('.sidebar')).toBeVisible();
    }
  });
}

test('searches with Enter, locates the exact item and loads raw events on demand', async ({ page }) => {
  const rawRequests = await mockApi(page);
  await page.goto('/');
  const search = page.getByRole('searchbox', { name: '搜索消息和工具摘要' });
  await search.fill('fixture');
  await search.press('Enter');
  await expect(page.getByText('safe matching snippet')).toBeVisible();
  await page.getByText('safe matching snippet').click();
  await expect(page.locator('[data-item-id="item-fast"]')).toBeVisible();
  expect(rawRequests()).toBe(0);
  await page.getByText('Raw Inspector（已脱敏，按需加载）').click();
  await expect.poll(rawRequests).toBe(1);
  await expect(page.locator('.raw-event').getByText('event 8')).toBeVisible();
  expect(await page.locator('.raw-event img').count()).toBe(0);
});

test('discards a slow stale Thread response after a faster selection', async ({ page }) => {
  await mockApi(page, { [longId]: 250, 'thread-fast': 5 });
  await page.goto('/');
  await page.getByText('Slow thread').first().click();
  await page.getByText('Fast thread').first().click();
  await expect(page.getByRole('heading', { name: 'Fast thread' })).toBeVisible();
  await page.waitForTimeout(300);
  await expect(page.getByRole('heading', { name: 'Slow thread' })).toHaveCount(0);
});

test('keeps filters and healthy process details out of the default reading path', async ({ page }) => {
  await mockApi(page);
  await page.goto('/');
  await expect(page.locator('.filters')).not.toBeVisible();
  await page.getByText('Fast thread').first().click();
  await expect(page.getByText('fixture question')).toBeVisible();
  await expect(page.getByText('fixture answer')).toBeVisible();
  const process = page.locator('.activity-panel');
  await expect(process).not.toHaveAttribute('open', '');
  await process.locator('> summary').click();
  await expect(page.getByText('command output')).toBeVisible();
});

test('shows projectless conversations in Recent without exposing generated cwd names as projects', async ({ page }) => {
  await mockApi(page);
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '最近' })).toBeVisible();
  await expect(page.getByText('Recent conversation')).toBeVisible();
  await expect(page.locator('.project-group')).toHaveCount(1);
  await expect(page.getByText('generated-name')).toHaveCount(0);
});

test('controls a live thread with image Composer, interrupt, approval and cursor reconnect', async ({ page }) => {
  const capture = await mockControlApi(page);
  await page.goto('/');
  await page.getByText('Fast thread').first().click();
  await expect(page.getByLabel('Codex 控制 Composer')).toContainText('在当前 Turn 中追加指令');
  await page.locator('.image-picker input').setInputFiles({ name:'fixture.png',mimeType:'image/png',buffer:Buffer.from('\x89PNG\r\n\x1a\nfixture') });
  await expect(page.locator('.image-preview')).toContainText('fixture.png');
  await page.getByPlaceholder('继续引导当前任务…').fill('image message');
  await expect(page.getByRole('button', { name:'停止当前 Turn' })).toBeVisible();
  await expect(page.getByRole('button', { name:'发送追加指令', exact:true })).toBeVisible();
  await page.getByRole('button', { name:'发送追加指令', exact:true }).click();
  await expect.poll(() => capture.uploadedBytes).toBeGreaterThan(8);
  await expect.poll(() => capture.input?.uploadIds).toEqual(['upload-1']);
  expect(capture.input?.sourceEpoch).toBe('epoch-1');
  await expect(page.locator('.optimistic-message')).toContainText('image message');
  await expect(page.locator('.optimistic-message')).toContainText('等待投影');
  await expect(page.locator('.optimistic-message')).toHaveCount(0, { timeout:4_000 });

  const composer = page.getByPlaceholder('继续引导当前任务…');
  await composer.press('ArrowUp');
  await expect(composer).toHaveValue('image message');
  await composer.fill('中断时保留这段草稿');
  await composer.press('Escape');
  await expect.poll(() => capture.input?.text).toBe('/interrupt');
  await expect(composer).toHaveValue('中断时保留这段草稿');
  await expect(page.getByText('cargo test')).toBeVisible();
  await expect(page.getByText('/fixture/project')).toBeVisible();
  await page.getByRole('button', { name:'允许', exact:true }).click();
  await expect.poll(() => capture.action).toMatchObject({ sourceEpoch:'epoch-1',expectedRequestVersion:1,action:{type:'approval',decision:'accept'} });
  await expect.poll(() => capture.streamPaths.some((path) => new URL(path, 'http://fixture').searchParams.get('cursor') === 'signed-stream-cursor')).toBe(true);

  await page.getByText('Slow thread').first().click();
  await expect(page.getByRole('heading', { name:'Slow thread' })).toBeVisible();
  await expect(page.getByPlaceholder('继续引导当前任务…')).toHaveValue('');
});

test('shows outcome_unknown as non-replayed uncertainty', async ({ page }) => {
  await mockControlApi(page);
  await page.goto('/');
  await page.getByText('Fast thread').first().click();
  await page.getByPlaceholder('继续引导当前任务…').fill('simulate uncertainty');
  await page.getByRole('button', { name:'发送追加指令', exact:true }).click();
  await expect(page.locator('.notice')).toContainText('操作结果未知');
  await expect(page.locator('.notice')).toContainText('不会自动重放');
});

test('creates a new exact-epoch Thread and changes only catalog-advertised settings', async ({ page }) => {
  const capture = await mockControlApi(page);
  await page.goto('/');
  await page.getByRole('button', { name:'新建对话' }).click();
  const dialog = page.getByRole('dialog', { name:'新建 Codex Thread' });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel('cwd').fill('/fixture/project');
  await dialog.getByLabel('New thread model').selectOption('fixture');
  await dialog.getByLabel('New thread permissions').selectOption('workspace');
  await dialog.getByRole('button', { name:'创建 Thread' }).click();
  await expect.poll(() => capture.createdThread).toMatchObject({sourceId:'source-1',sourceEpoch:'epoch-1',cwd:'/fixture/project',model:'fixture',permissions:'workspace'});
  await expect(dialog).toBeHidden();

  await page.getByText('Fast thread').first().click();
  await page.locator('.control-settings > summary').click();
  await page.getByLabel('Reasoning').selectOption('high');
  await expect.poll(() => capture.setting).toMatchObject({capability:'thread.settings.reasoning',target:{sourceId:'source-1',sourceEpoch:'epoch-1',threadKey:'thread-fast'},input:{value:'high'}});
  await page.getByRole('button', { name:'进入 Plan' }).click();
  await expect.poll(() => capture.input?.text).toBe('/plan');
});

test('executes /clear as an audited new Thread and switches away from the old conversation', async ({ page }) => {
  const capture = await mockControlApi(page, false);
  await page.goto('/');
  await page.getByText('Fast thread').first().click();
  const composer = page.getByPlaceholder('询问 Codex，输入 / 查看命令');
  await composer.fill('/clear');
  await composer.press('Enter');
  await expect.poll(() => capture.createdThread).toMatchObject({sourceId:'source-1',sourceEpoch:'epoch-1',model:'fixture'});
  expect(capture.input).toBeUndefined();
  await expect(page.getByRole('heading',{name:'Cleared thread'})).toBeVisible();
  await expect(page.locator('.notice')).toContainText('已清空当前界面并开始新对话');
});

test('redeems a pairing fragment, clears it, and does not persist a bearer token', async ({ page }) => {
  let paired = false;
  let suppliedCode = '';
  await page.route('**/v1/**', async (route) => {
    const url = new URL(route.request().url());
    if (url.pathname === '/v1/auth/pair') {
      suppliedCode = JSON.parse(route.request().postData() || '{}').code;
      paired = true;
      await route.fulfill({ status: 200, contentType: 'application/json', headers: { 'set-cookie': 'observer_session=fixture; HttpOnly; SameSite=Strict; Path=/' }, body: JSON.stringify(envelope({ paired: true })) });
      return;
    }
    if (!paired) {
      await route.fulfill({ status: 401, contentType: 'application/json', body: JSON.stringify({ error: { message: 'pair first' } }) });
      return;
    }
    const data = url.pathname === '/v1/health' ? { status: 'healthy', ready: true }
      : url.pathname === '/v1/projects' || url.pathname === '/v1/sources' || url.pathname === '/v1/threads' ? [] : [];
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(envelope(data)) });
  });
  await page.goto('/#pair=signed-fixture-code');
  await expect.poll(() => suppliedCode).toBe('signed-fixture-code');
  await expect(page.getByText('尚未导入或没有符合筛选条件的 Thread。')).toBeVisible();
  expect(new URL(page.url()).hash).toBe('');
  expect(await page.evaluate(() => sessionStorage.getItem('observer-token'))).toBeNull();
});

test('opens xterm preview, sends leased input, and reattaches after refresh on narrow screens', async ({ page }) => {
  const fixture = await mockSessionKernel(page);
  await page.goto('/');
  await page.getByRole('button',{name:'终端会话'}).click();
  const dialog = page.getByRole('dialog',{name:'Codex terminal session'});
  await dialog.getByLabel('cwd').fill('/fixture/project');
  await dialog.getByRole('button',{name:'启动原生 TUI'}).click();
  await expect(dialog.getByText('xterm 已连接')).toBeVisible();
  await expect(dialog.getByText('可输入')).toBeVisible();
  await dialog.locator('.xterm-helper-textarea').pressSequentially('hello');
  await expect.poll(() => page.evaluate(() => (window as unknown as { terminalFrames: string[] }).terminalFrames
    .some((frame) => JSON.parse(frame).type === 'input'))).toBe(true);

  await page.setViewportSize({width:390,height:780});
  await page.reload();
  await page.getByRole('button',{name:'终端会话'}).click();
  await expect(page.getByText('xterm 已连接')).toBeVisible();
  await expect.poll(fixture.getAttachCount).toBeGreaterThan(1);
  await expect.poll(fixture.getResumeControlValidated).toBe(true);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  const stop = dialog.getByRole('button',{name:'停止 Worker'});
  expect(await stop.evaluate((button) => {
    const bounds = button.getBoundingClientRect();
    const hit = document.elementFromPoint(
      bounds.left + bounds.width / 2,
      bounds.top + bounds.height / 2
    );
    return {
      same: hit === button,
      hitClass: hit?.getAttribute('class'),
      hitText: hit?.textContent?.trim().slice(0,80),
      buttonBounds: { left:bounds.left,top:bounds.top,right:bounds.right,bottom:bounds.bottom }
    };
  })).toMatchObject({same:true});
  await stop.click();
  await expect(dialog.getByText('stopping',{exact:true})).toBeVisible();
});

test('keeps a second xterm attachment read-only when another attachment owns input', async ({ page }) => {
  await mockSessionKernel(page, true);
  await page.goto('/');
  await page.getByRole('button',{name:'终端会话'}).click();
  const dialog = page.getByRole('dialog',{name:'Codex terminal session'});
  await dialog.getByLabel('cwd').fill('/fixture/project');
  await dialog.getByRole('button',{name:'启动原生 TUI'}).click();
  await expect(dialog.getByText('xterm 已连接')).toBeVisible();
  await expect(dialog.getByText('只读',{exact:true})).toBeVisible();
  await expect(dialog.getByRole('alert')).toContainText('另一个浏览器持有输入租约');
});

test('keeps IME-style Unicode, multiline paste, resize and disconnect recovery inside xterm', async ({ page }) => {
  const fixture = await mockSessionKernel(page);
  await page.goto('/');
  await page.getByRole('button',{name:'终端会话'}).click();
  const dialog = page.getByRole('dialog',{name:'Codex terminal session'});
  await dialog.getByLabel('cwd').fill('/fixture/project');
  await dialog.getByRole('button',{name:'启动原生 TUI'}).click();
  await expect(dialog.getByText('xterm 已连接')).toBeVisible();
  await expect(dialog.getByText('可输入')).toBeVisible();

  const terminal = dialog.locator('.xterm-helper-textarea');
  await terminal.focus();
  await page.keyboard.insertText('输入🙂');
  await terminal.evaluate((element) => {
    const transfer = new DataTransfer();
    transfer.setData('text/plain', '第一行\n第二行');
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData:transfer, bubbles:true }));
  });
  await expect.poll(() => page.evaluate(() => (window as unknown as { terminalFrames: string[] }).terminalFrames
    .map((frame) => JSON.parse(frame))
    .filter((frame) => frame.type === 'input')
    .map((frame) => frame.data)
    .join(''))).toContain('输入🙂');
  await expect.poll(() => page.evaluate(() => (window as unknown as { terminalFrames: string[] }).terminalFrames
    .map((frame) => JSON.parse(frame))
    .filter((frame) => frame.type === 'input')
    .map((frame) => frame.data)
    .join(''))).toContain('第一行\r第二行');

  await page.setViewportSize({width:760,height:720});
  await expect.poll(() => page.evaluate(() => (window as unknown as { terminalFrames: string[] }).terminalFrames
    .some((serialized) => JSON.parse(serialized).type === 'resize'))).toBe(true);
  await page.evaluate(() => (window as unknown as { pushTerminalOutput?: (text: string, seq: number) => void })
    .pushTerminalOutput?.(`安全中文🙂${'超长'.repeat(5000)}\r\n\x1b]8;;javascript:alert(1)\x07不可执行链接\x1b]8;;\x07`, 100));
  await expect(dialog.locator('.terminal-panel script, .terminal-panel img, .terminal-panel a[href^="javascript:"]')).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(760);

  await page.evaluate(() => (window as unknown as { disconnectTerminal?: () => void }).disconnectTerminal?.());
  await expect.poll(fixture.getAttachCount).toBeGreaterThan(1);
  await expect(dialog.getByText('xterm 已连接')).toBeVisible();
  await expect.poll(fixture.getResumeControlValidated).toBe(true);
});

test('uses native TUI as the default mutation path in tui mode', async ({ page }) => {
  await mockSessionKernel(page, false, 'tui');
  await page.goto('/');
  await page.getByText('Fast thread').first().click();
  await expect(page.getByRole('heading', { name: 'Fast thread' })).toBeVisible();
  await expect(page.getByLabel('Codex 控制 Composer')).toHaveCount(0);
  await expect(page.getByText('活动输入由真实 Codex TUI 承载')).toBeVisible();
  await page.getByRole('button', { name: '继续终端会话' }).click();
  await expect(page.getByRole('dialog', { name: 'Codex terminal session' })).toBeVisible();
});

test('resumes through native TUI and sends Slash, picker keys and interrupt only to the owned session', async ({ page }) => {
  const fixture = await mockSessionKernel(page, false, 'tui', true);
  await page.goto('/');
  await page.getByText('Fast thread').first().click();
  await page.getByRole('button', { name: '继续终端会话' }).click();
  const dialog = page.getByRole('dialog', { name: 'Codex terminal session' });
  await expect(dialog.getByLabel('Codex Thread ID')).toHaveValue('thread-fast');
  await dialog.getByRole('button', { name: '恢复原生 TUI' }).click();
  await expect(dialog.getByText('xterm 已连接')).toBeVisible();
  await expect.poll(() => fixture.getCreateBodies()[0]).toMatchObject({
    mode:'resume',codexThreadId:'thread-fast',sourceEpoch:'epoch-1'
  });

  const terminal = dialog.locator('.xterm-helper-textarea');
  await terminal.pressSequentially('/clear');
  await terminal.press('Enter');
  await terminal.pressSequentially('/goal ship safely');
  await terminal.press('Enter');
  await terminal.press('/');
  await terminal.press('ArrowDown');
  await terminal.press('Enter');
  const input = await page.evaluate(() => (window as unknown as { terminalFrames: string[] }).terminalFrames
    .map((frame) => JSON.parse(frame))
    .filter((frame) => frame.type === 'input')
    .map((frame) => frame.data)
    .join(''));
  expect(input).toContain('/clear');
  expect(input).toContain('/goal ship safely');
  expect(input).toContain('\r');

  await dialog.getByRole('button', { name: /Interrupt turn-act/ }).click();
  await expect.poll(fixture.getInterruptBody).toMatchObject({
    sourceEpoch:'epoch-1',threadId:'thread-fast',expectedTurnId:'turn-active',expectedWorkerVersion:4
  });
  await expect(dialog.getByText(/interrupt · completed/)).toBeVisible();
});
