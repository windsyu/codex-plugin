// Isolated native CLI + installed Chrome: two projects share only Application.
const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
const jsQR = require('jsqr');
let stage = 'home';
setTimeout(() => { console.log(JSON.stringify({ stage: 'failed', check: stage, reason: 'deadline' })); close(1); }, 115000).unref();
const terminal = page => page.locator('.xterm-helper-textarea');
const screen = page => page.locator('.xterm-rows').innerText();
const app = page => page.evaluate(async () => (await fetch('/workbench/v1/application')).json());
const snapshot = (page, id) => page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/live/snapshot`)).json(), id);
async function ready(page) {
  const priorStage = stage; stage = `${priorStage}-terminal-ready`;
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  stage = `${priorStage}-ownership`;
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  stage = `${priorStage}-native-ready`;
  for (let n = 0; n < 100; n++) {
    const text = await screen(page);
    if (/Choose your style|Select a theme|Do you trust|Do you want to work|Trust this folder/.test(text)) await terminal(page).press('Enter');
    else if (text.includes('OpenAI Codex') && text.includes('›')) return;
    await page.waitForTimeout(150);
  }
  throw new Error('native not ready');
}
async function paste(page, value) {
  await terminal(page).evaluate((el, value) => { const data = new DataTransfer(); data.setData('text/plain', value); el.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true })); }, value);
}
async function input(page, label) {
  await paste(page, `R6_F_${label}_INPUT 请用中文回复本项目。`);
  await terminal(page).press('Enter');
}
async function prepare(page, path) {
  await page.getByRole('button', { name: '输入路径', exact: true }).click();
  await page.getByLabel('项目目录', { exact: true }).fill(path);
  const result = page.waitForResponse(r => r.url().endsWith('/workbench/v1/launch-targets') && r.request().method() === 'POST');
  await page.getByLabel('项目目录', { exact: true }).press('Enter');
  const value = await (await result).json();
  await expect(page.getByRole('button', { name: /开始新对话|进入工作台/, exact: true })).toBeEnabled();
  return value;
}
async function accessQr(page) {
  let decoded;
  await expect.poll(async () => {
    const pixels = await page.locator('.wb-access-qr canvas').evaluate(canvas => ({
      data: Array.from(canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data),
      width: canvas.width, height: canvas.height,
    }));
    decoded = jsQR(new Uint8ClampedArray(pixels.data), pixels.width, pixels.height);
    return !!decoded;
  }).toBe(true);
  return decoded.data;
}
async function phoneAccess(browser, a, b, runA, runB) {
  const phones = []; let phoneErrors = 0;
  async function enable(page, run) {
    await page.getByRole('button', { name: '手机接入', exact: true }).click();
    await page.getByRole('button', { name: '开启设备访问', exact: true }).click();
    await expect(page.locator('.wb-access-qr canvas')).toBeVisible();
    const url = await accessQr(page);
    // Use the actual UI output: constructing a URL from the API hides regressions.
    assert.equal(new URL(url).searchParams.get('run'), run.runId, 'QR must preserve the Run selector');
    assert.ok(new URL(url).hash.startsWith('#pair='), 'QR must contain a pairing fragment');
    return url;
  }
  async function open(url, run) {
    const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
    phones.push(context);
    const page = await context.newPage(); page.setDefaultTimeout(10000); page.on('pageerror', () => phoneErrors++);
    await page.goto(url);
    await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
    assert.equal(new URL(page.url()).hash, '');
    const info = await page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/run`)).json(), run.runId);
    assert.equal(info.runEpoch, run.runId); assert.equal(info.processId, run.cliPid);
    await expect(page.getByRole('button', { name: '手机接入', exact: true })).toHaveCount(0);
    await expect(page.locator('#wb-settings-button')).toHaveCount(0);
    await expect(page.getByRole('link', { name: '返回首页', exact: true })).toHaveCount(0);
    return { page, context };
  }
  try {
    stage = 'device-qr-a'; const urlA = await enable(a, runA);
    stage = 'device-pair-a'; const phoneA = await open(urlA, runA);
    stage = 'device-qr-b'; const urlB = await enable(b, runB);
    assert.equal(new URL(urlA).origin, new URL(urlB).origin, 'Runs share one device listener');
    stage = 'device-pair-b'; const phoneB = await open(urlB, runB);
    stage = 'device-scope-isolation';
    const denied = await phoneA.page.evaluate(async ({ a, b }) => {
      const paths = ['/workbench/v1/application', '/workbench/v1/library/entries', '/workbench/v1/library/sources', '/workbench/v1/settings',
        `/workbench/v1/runs/${a}/settings`, `/workbench/v1/runs/${b}/run`, `/workbench/v1/runs/${b}/live/snapshot`];
      return Promise.all(paths.map(async path => (await fetch(path)).status));
    }, { a: runA.runId, b: runB.runId });
    assert.ok(denied.every(status => [401, 403, 404].includes(status)), 'device credentials must not grant other Run or global access');
    stage = 'device-refresh-repeat';
    const cookies = await phoneA.context.cookies();
    await phoneA.page.reload();
    await expect(phoneA.page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
    await phoneA.page.goto(urlA);
    await expect(phoneA.page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
    assert.ok(JSON.stringify(await phoneA.context.cookies()) === JSON.stringify(cookies), 'repeat scan must preserve the existing Cookie');
    stage = 'device-close-a-keeps-b';
    await expect(a.locator('.wb-access-device')).toHaveCount(1);
    await expect(b.locator('.wb-access-device')).toHaveCount(1);
    await a.getByRole('button', { name: '关闭设备访问', exact: true }).click();
    await expect(a.getByRole('button', { name: '开启设备访问', exact: true })).toBeVisible();
    assert.equal(await phoneA.page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/run`)).status, runA.runId), 404);
    await phoneB.page.reload();
    await expect(phoneB.page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
    assert.equal(await phoneB.page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/run`)).status, runB.runId), 200);
    await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
    await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
    assert.equal(phoneErrors, 0, 'phone pages must not raise JavaScript errors');
    stage = 'device-close-b';
    await b.getByRole('button', { name: '关闭设备访问', exact: true }).click();
    await expect(b.getByRole('button', { name: '开启设备访问', exact: true })).toBeVisible();
  } finally {
    await Promise.all(phones.map(context => context.close()));
    for (const page of [a, b]) {
      if (await page.locator('#wb-access-panel').isVisible()) await page.locator('#wb-access-panel').getByRole('button', { name: '收起', exact: true }).click();
    }
  }
}
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, args: ['--no-proxy-server', '--host-resolver-rules=MAP * 127.0.0.1, EXCLUDE localhost'] });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  let errors = 0, starts = 0, takeovers = 0; const scoped = [];
  context.on('page', page => {
    page.setDefaultTimeout(15000); page.on('pageerror', () => errors++);
    page.on('request', r => { const path = new URL(r.url()).pathname; if (path === '/workbench/v1/runs' && r.method() === 'POST') starts++; if (/\/(live|terminal|requests|workspace|history|settings)(\/|$)/.test(path) && !path.includes('/library/') && !path.includes('/application/')) scoped.push(path); });
    page.on('websocket', socket => socket.on('framesent', ({ payload }) => {
      if (JSON.parse(payload).command?.type === 'takeover') takeovers++;
    }));
  });
  const a = await context.newPage(); await a.goto(process.env.WORKBENCH_PROBE_URL);
  assert.equal((await app(a)).runs.length, 0);
  stage = 'prepare-first'; await prepare(a, process.env.WORKBENCH_PROBE_PROJECT_A);
  stage = 'start-first'; await a.getByRole('button', { name: '开始新对话', exact: true }).click();
  stage = 'ready-first'; await ready(a);
  const runA = (await app(a)).runs[0];
  const home = await context.newPage(); await home.goto(new URL('/', a.url()).href);
  stage = 'second-project-popup';
  const target = await prepare(home, process.env.WORKBENCH_PROBE_PROJECT_B); assert.equal(target.openInNewTab, true);
  const popup = home.waitForEvent('popup');
  const operationResponse = home.waitForResponse(r => r.url().endsWith('/workbench/v1/runs') && r.request().method() === 'POST');
  await home.getByRole('button', { name: '开始新对话', exact: true }).click();
  const operation = await (await operationResponse).json();
  const b = await popup; await ready(b);
  const completedOperation = await home.evaluate(async id => (await fetch(`/workbench/v1/launch-operations/${id}`)).json(), operation.operationId);
  assert.equal(completedOperation.openInNewTab, true);
  const runs = (await app(home)).runs; assert.equal(runs.length, 2);
  const runB = runs.find(r => r.runId !== runA.runId);
  assert.equal(runB.projectPath, process.env.WORKBENCH_PROBE_PROJECT_B);
  assert.notEqual(runA.cliPid, runB.cliPid); assert.equal(runs.find(r => r.runId === runA.runId).cliPid, runA.cliPid);
  assert.equal(new URL(a.url()).searchParams.get('run'), runA.runId);
  assert.equal(new URL(b.url()).searchParams.get('run'), runB.runId);
  await phoneAccess(browser, a, b, runA, runB);
  stage = 'parallel-native-streams';
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  assert.equal(takeovers, 0, 'opening another project must not require any input takeover');
  await Promise.all([input(a, 'A'), input(b, 'B')]);
  for (const [page, label, other] of [[a, 'A', 'B'], [b, 'B', 'A']]) {
    await expect(page.locator('.wb-message-list')).toContainText(`R6_F_${label}_中文流式`);
    assert.ok(!(await page.locator('.wb-message-list').innerText()).includes(`R6_F_${label}_DONE`));
    await expect(page.locator('.wb-message-list')).not.toContainText(`R6_F_${other}`);
  }
  stage = 'history-refresh-during-parallel-streams';
  assert.equal(await home.evaluate(async () => (await fetch('/workbench/v1/library/refresh', { method: 'POST' })).status), 202);
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  assert.equal(takeovers, 0);
  for (const [page, label, run, total] of [[a, 'A', runA, 110], [b, 'B', runB, 210]]) {
    await expect(page.locator('.wb-message-list')).toContainText(`R6_F_${label}_DONE`);
    await expect.poll(() => screen(page)).toContain(`R6_F_${label}_DONE`);
    stage = `native-tool-card-${label}`;
    const card = page.locator('.wb-message-list .wb-tool-card').first();
    await expect(card.locator('.wb-tool-result')).toContainText(`R6_F_${label}_FILE`);
    await expect(card.locator('.wb-tool-result')).not.toContainText(`R6_F_${label === 'A' ? 'B' : 'A'}_FILE`);
    const output = card.locator('.wb-tool-result details');
    if (await output.getAttribute('open') === null) await output.locator('summary').click();
    await expect(output.locator('pre')).toBeVisible();
    if (process.env.WORKBENCH_PROBE_SCREENSHOT) await page.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.${label}.png` });
    stage = `files-and-usage-${label}`;
    await page.getByRole('button', { name: '文件', exact: true }).click();
    await page.getByRole('treeitem').filter({ hasText: 'R6-check.txt' }).click();
    await expect(page.getByRole('region', { name: '代码内容', exact: true })).toContainText(`R6_F_${label}_FILE`);
    await expect.poll(async () => (await snapshot(page, run.runId)).usageSummary.totalTokens.tokens).toBe(total);
    await expect.poll(async () => { const s = await snapshot(page, run.runId); return s.recorder === 'saved' && s.persistedThroughViewSeq === s.viewSeq; }).toBe(true);
    const saved = await page.evaluate(async id => (await fetch(`/workbench/v1/runs/${id}/history/${id}`)).json(), run.runId);
    assert.equal(saved.snapshot.usageSummary.totalTokens.tokens, total);
    assert.ok(!JSON.stringify(saved).includes(`R6_F_${label === 'A' ? 'B' : 'A'}_`));
  }
  stage = 'history-refresh-completed-with-runs-isolated';
  await expect.poll(async () => {
    const info = await app(home);
    const workbench = info.sources.find(source => source.id === 'default-workbench');
    return workbench?.indexedEntries >= 2 && info.sources.every(source => ['ready', 'partial', 'read_only'].includes(source.state));
  }).toBe(true);
  assert.deepEqual((await app(home)).runs.map(run => [run.runId, run.cliPid]).sort(), [[runA.runId, runA.cliPid], [runB.runId, runB.cliPid]].sort());
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  const deniedHistory = await a.evaluate(async ({ a, b }) => (await fetch(`/workbench/v1/runs/${a}/history/${b}`)).status, { a: runA.runId, b: runB.runId });
  assert.equal(deniedHistory, 404);
  stage = 'same-project-reuse';
  await home.keyboard.press('Escape'); const existing = await prepare(home, process.env.WORKBENCH_PROBE_PROJECT_A);
  stage = 'same-project-preview'; assert.equal(existing.existingRun.runId, runA.runId);
  assert.equal(existing.openInNewTab, true);
  const reusedPopup = home.waitForEvent('popup');
  await home.getByRole('button', { name: '进入工作台', exact: true }).click();
  stage = 'same-project-popup'; const reused = await reusedPopup;
  await expect(reused.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  assert.equal(new URL(reused.url()).searchParams.get('run'), runA.runId);
  await expect(reused.locator('.wb-terminal')).toHaveAttribute('data-owned', 'false');
  await expect(reused.locator('.wb-terminal')).toContainText('同一工作台的另一页面');
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  assert.equal(takeovers, 0);
  stage = 'same-run-takeover-keeps-other-project-writable';
  await reused.getByRole('button', { name: '在此输入', exact: true }).click();
  await expect(reused.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'false');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await paste(b, 'R6_F_B_DRAFT 另一个项目仍可输入');
  await expect.poll(() => screen(b)).toContain('R6_F_B_DRAFT');
  assert.ok(!(await screen(reused)).includes('R6_F_B_DRAFT'));
  await terminal(b).press('Control+u');
  await a.getByRole('button', { name: '在此输入', exact: true }).click();
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(reused.locator('.wb-terminal')).toHaveAttribute('data-owned', 'false');
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  stage = 'reload-one-project-keeps-other-input';
  await b.reload();
  await expect(b.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  assert.equal(takeovers, 2, 'only explicit same-Run switches may take over input');
  await reused.close();
  assert.equal((await app(home)).runs.length, 2); assert.equal(starts, 2);
  await home.keyboard.press('Escape');
  await expect(a.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true');
  if (process.env.WORKBENCH_PROBE_SCREENSHOT) await home.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.home.png` });
  stage = 'targeted-stop';
  await b.getByRole('button', { name: '终端', exact: true }).click();
  home.once('dialog', dialog => dialog.accept());
  await home.getByRole('button', { name: `停止项目 ${runB.projectName}`, exact: true }).click();
  await expect.poll(async () => (await app(home)).runs.find(r => r.runId === runB.runId)?.state).toBe('stopped');
  await expect(b.locator('.wb-terminal-status')).toHaveText('已结束');
  stage = 'centered-exit-notice';
  const notice = b.getByRole('dialog', { name: '本次运行已结束', exact: true });
  await expect(notice).toBeVisible();
  stage = 'exit-focus';
  await expect(notice.getByRole('button', { name: '继续查看记录' })).toBeFocused();
  await expect(a.getByRole('dialog', { name: '本次运行已结束', exact: true })).toHaveCount(0);
  await b.keyboard.press('Escape');
  stage = 'exit-dismiss-and-focus';
  await expect(notice).toHaveCount(0);
  stage = 'exit-restored-focus';
  await expect(b.getByRole('button', { name: '终端', exact: true })).toBeFocused();
  stage = 'exit-retained-records';
  await expect(b.locator('.wb-message-list')).toContainText('R6_F_B_DONE');
  // A fresh page of an ended Run receives its exit in the terminal snapshot.
  await b.reload();
  stage = 'exit-snapshot';
  await expect(notice).toBeVisible();
  for (const width of [1440, 390, 320]) {
    stage = `exit-layout-${width}`;
    await b.setViewportSize({ width, height: 900 });
    const bounds = await notice.boundingBox();
    assert.ok(bounds && Math.abs(bounds.x + bounds.width / 2 - width / 2) < 2);
    assert.ok(Math.abs(bounds.y + bounds.height / 2 - 450) < 2);
    assert.ok(await notice.evaluate(el => el.scrollWidth <= el.clientWidth));
    assert.ok(await b.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
    if (process.env.WORKBENCH_PROBE_SCREENSHOT) await b.screenshot({ path: `${process.env.WORKBENCH_PROBE_SCREENSHOT}.exit-${width}.png` });
  }
  for (let n = 0; n < 7; n++) {
    stage = `exit-focus-trap-${n}`;
    await b.keyboard.press('Tab');
    // Native dialogs may let Tab reach browser chrome (body becomes active),
    // but never the inert workbench controls behind the modal.
    assert.ok(await notice.evaluate(el => el.matches(':modal') && (el.contains(document.activeElement) || document.activeElement === document.body)));
  }
  await notice.getByRole('button', { name: '继续查看记录' }).click();
  stage = 'exit-keep-records';
  await expect(notice).toHaveCount(0);
  await expect(b.locator('.wb-message-list')).toContainText('R6_F_B_DONE');
  await b.getByRole('button', { name: '查看运行结束提示' }).click();
  stage = 'exit-return-home';
  await expect(notice).toBeVisible();
  await notice.getByRole('link', { name: '返回首页' }).click();
  await expect(b.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  assert.equal(starts, 2, 'viewing or dismissing exit notices never starts another CLI');
  assert.equal((await app(home)).runs.find(r => r.runId === runA.runId).cliPid, runA.cliPid);
  stage = 'survivor-input'; await input(a, 'A'); await expect.poll(() => screen(a)).toContain('R6_F_A_DONE_4');
  await home.reload(); await expect(home.getByRole('heading', { name: '全部历史', exact: true })).toBeVisible();
  assert.equal(errors, 0); assert.ok(scoped.length > 5); assert.ok(scoped.every(path => /^\/workbench\/v1\/runs\/[a-f0-9-]+\//.test(path)));
  require('node:fs').writeFileSync(process.env.WORKBENCH_PROBE_RESULT, JSON.stringify({ pids: [runA.cliPid, runB.cliPid] }));
  console.log(JSON.stringify({ stage: 'passed', starts, projects: 2, deviceQrScopedPairing: true, deviceRepeatRefresh: true, deviceRunIsolation: true, deviceCloseKeepsOtherRun: true, parallelStreams: true, historyRefreshDuringParallelStreams: true, simultaneousInputOwners: true, crossProjectTakeovers: 0, sameRunTakeoverIsolated: true, reloadKeepsOtherInput: true, filesAndUsageIsolated: true, nativeToolOutputsIsolated: true, journalsSaved: true, targetStopIsolated: true, centeredExitNotice: true, exitViewports: [1440, 390, 320], exitKeyboardAndRecords: true, runScopedClients: true, pageErrors: errors, browser: browser.version(), cliVersion: runA.cliVersion })); await close(0);
})().catch(error => { console.log(JSON.stringify({ stage: 'failed', check: stage, reason: String(error.message).replace(/https?:\/\/\S+/g, '[url]').slice(0, 500) })); close(1); });
