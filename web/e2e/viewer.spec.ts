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
    if (path === '/v1/health') data = { status: 'healthy', ready: true, privacy: { legacyRedactionEvents: 1, warning: 'legacy warning' } };
    else if (path === '/v1/projects') data = [{ project: { key: 'project', name: 'Fixture project', path: '/fixture' }, threadCount: 2, currentThreadCount: 2, lastRecencyAtMs: 8 }];
    else if (path === '/v1/sources') data = [{ sourceId: 'source-1', kind: 'rollout', stableIdentity: 'fixture', status: 'ready', currentEpoch: { decodeErrorCount: 1, unknownEventCount: 1 } }];
    else if (path === '/v1/threads') data = [thread(longId, 'Slow thread'), thread('thread-fast', 'Fast thread')];
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
  await expect(page.getByText('尚未导入或没有符合筛选条件的 Thread。')).toBeVisible();
  expect(suppliedCode).toBe('signed-fixture-code');
  expect(new URL(page.url()).hash).toBe('');
  expect(await page.evaluate(() => sessionStorage.getItem('observer-token'))).toBeNull();
});
