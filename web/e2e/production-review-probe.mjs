// Independent regression checks for the production UI review findings.
// Uses current Vite sources, synthetic loopback responses and system Chrome.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import { chromium, expect } from '@playwright/test';
import { createRun } from '../../scripts/lib/artifacts.mjs';
import { productFixture, runId, snapshot } from './product-design-fixture.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const artifact = createRun(repo, { label: 'independent-production-ui-review-regressions' });
const runtime = path.join(artifact.path, 'isolated-browser');
fs.mkdirSync(runtime, { mode: 0o700 });
const fixture = productFixture(repo);
const result = { checks: [], observations: [], pageErrors: [], externalRequests: [], reviewedFiles: {}, scope: 'Production Preact with synthetic loopback API/WS; no real account, relay, CLI or private data.' };
for (const name of ['App.tsx', 'main.tsx', 'FileReading.tsx', 'session.css']) {
  result.reviewedFiles[name] = crypto.createHash('sha256').update(fs.readFileSync(path.join(repo, 'web/src/workbench', name))).digest('hex');
}
let browser, server, stage = 'startup', success = false;
try {
  server = await createServer({ root: path.join(repo, 'web'), configFile: path.join(repo, 'web/vite.config.ts'), plugins: [fixture.plugin], server: { host: '127.0.0.1', port: 0 }, logLevel: 'error' });
  await server.listen();
  const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
  browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--no-proxy-server'], env: { ...process.env, HOME: runtime, USERPROFILE: runtime, CODEX_HOME: path.join(runtime, 'codex-home'), TMPDIR: runtime } });
  result.browser = browser.version();
  const context = await browser.newContext({ viewport: { width: 390, height: 960 } });
  let standalone = false, crossRun = false, delayNextRun = false, pendingRun, releaseRun;
  const secondRun = '44444444-4444-4444-8444-444444444444';
  const secondPrefix = `/workbench/v1/runs/${secondRun}`;
  const secondRequests = [];
  await context.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) { result.externalRequests.push(url.origin + url.pathname); return route.abort(); }
    if (crossRun && url.pathname === '/workbench/v1/application') {
      const response = await route.fetch();
      const data = await response.json();
      data.runs.push({ runId: secondRun, projectName: 'Review Run B', projectPath: '/synthetic/b', state: 'running' });
      return route.fulfill({ response, json: data });
    }
    if (crossRun && url.pathname.startsWith(secondPrefix + '/')) {
      secondRequests.push(url.pathname);
      if (url.pathname === secondPrefix + '/run') {
        if (delayNextRun) { delayNextRun = false; pendingRun?.(); await new Promise(resolve => { releaseRun = resolve; }); }
        // A delayed request may have been aborted by navigation back to A.
        return route.fulfill({ json: { runEpoch: secondRun, projectName: 'Review Run B', processId: null, terminalAvailable: false, workspaceRoot: '/synthetic/b', historyAvailable: false, settingsAvailable: false } }).catch(() => {});
      }
      if (url.pathname === secondPrefix + '/live/snapshot') return route.fulfill({ json: snapshot(secondRun) });
      if (url.pathname === secondPrefix + '/live/events') return route.fulfill({ contentType: 'text/event-stream', body: ': synthetic B\n\n' });
      if (url.pathname === secondPrefix + '/workspace/files') return route.fulfill({ json: { path: '', entries: [{ name: 'B.txt', path: 'B.txt', kind: 'file' }], nextCursor: null, omitted: 0, readAt: '2026-11-17T00:00:00Z', elapsedMs: 1, truncated: false } });
      if (url.pathname === secondPrefix + '/workspace/file') return route.fulfill({ json: { path: 'B.txt', text: 'Only Run B content', revision: 'b1', readAt: '2026-11-17T00:00:00Z', elapsedMs: 1, truncated: false } });
      throw new Error('Unexpected second Run request: ' + url.pathname);
    }
    if (!standalone) return route.continue();
    if (url.pathname === '/workbench/v1/application') return route.fulfill({ status: 404, json: { error: 'standalone' } });
    if (url.pathname === '/workbench/v1/run') return route.fulfill({ json: { runEpoch: runId, projectName: 'Standalone review fixture', processId: null, terminalAvailable: false, workspaceRoot: '/synthetic/projects/design-workbench', historyAvailable: false, settingsAvailable: false } });
    if (url.pathname === '/workbench/v1/live/events') return route.fulfill({ contentType: 'text/event-stream', body: ': synthetic fixture\n\n' });
    if (url.pathname === '/workbench/v1/live/snapshot') {
      const data = snapshot();
      const key = { requestId: 'synthetic-request', responseId: 'synthetic-response', wireItemId: 'read-tool', contentIndex: 0 };
      data.items.push({ kind: 'tool_call', itemKey: 'read-tool', key, revision: 1, orderIndex: 2, completeness: 'observed', evidence: [{ origin: 'model', key, captureSeq: 3 }], captureSeq: 3, truncated: false, toolKind: 'function', callId: 'read-call', name: 'read_file', namespace: null, category: 'other', arguments: '{"path":"README.md"}', argumentsState: 'generated', command: null, execution: 'unobserved', result: null, proposedPatch: null, identityConflict: false, resultConflict: false });
      return route.fulfill({ json: data });
    }
    return route.continue();
  });
  const page = await context.newPage();
  page.on('pageerror', error => result.pageErrors.push(error.message));
  page.setDefaultTimeout(7000);
  stage = 'mobile-hidden-file-poll';
  await page.goto(`${origin}/runs/${runId}/files`);
  await page.getByRole('treeitem', { name: 'README.md', exact: true }).click();
  await expect(page.getByRole('region', { name: '文件阅读' })).toContainText('只读文件夹具');
  const terminal = await page.locator('.xterm').elementHandle();
  const connections = fixture.state.sockets;
  const inputCount = fixture.state.input.length;
  await page.getByRole('tab', { name: '终端', exact: true }).click();
  await expect(page.locator('#wb-session-reading')).toBeHidden();
  const count = () => fixture.state.requests.filter(request => request.path.endsWith('/workspace/file')).length;
  const before = count();
  await page.waitForTimeout(5500); // Cross the existing five-second polling boundary.
  const hiddenCount = count();
  assert.equal(hiddenCount, before, 'hidden mobile file must stop polling');
  await page.setViewportSize({ width: 1440, height: 960 });
  await expect(page.getByRole('region', { name: '文件阅读' })).toBeVisible();
  await expect.poll(count).toBeGreaterThan(hiddenCount);
  const resumedCount = count();
  await page.setViewportSize({ width: 390, height: 960 });
  await expect(page.locator('#wb-session-reading')).toBeHidden();
  await page.waitForTimeout(5500);
  assert.equal(count(), resumedCount, 'returning below mobile breakpoint must stop polling again');
  assert.ok(await terminal.evaluate(element => element === document.querySelector('.xterm')), 'view changes must preserve xterm');
  assert.equal(fixture.state.sockets, connections);
  assert.equal(fixture.state.input.length, inputCount);
  result.observations.push({ before, hiddenCount, resumedCount, afterSecondHide: count(), connections });
  await page.screenshot({ path: path.join(artifact.path, 'mobile-terminal-hidden-file.png') });
  result.checks.push('390px hidden file stops polling; 1440px resumes; returning to 390px stops; same terminal and no extra input');

  stage = 'standalone-file-link';
  standalone = true;
  await page.setViewportSize({ width: 1440, height: 960 });
  await page.goto(origin + '/');
  await expect(page.locator('.wb-file-link')).toBeVisible();
  await page.locator('.wb-file-link').click();
  await expect(page).toHaveURL(`${origin}/runs/${runId}/files`);
  await expect(page.getByRole('region', { name: '文件阅读' })).toBeVisible();
  await expect(page.getByRole('region', { name: '文件阅读' })).toContainText('只读文件夹具');
  await page.screenshot({ path: path.join(artifact.path, 'standalone-file-link.png') });
  result.checks.push('standalone root tool file link opens the current Run file route and visible reader');

  stage = 'cross-run-routing-and-late-metadata';
  standalone = false; crossRun = true;
  await page.goto(`${origin}/runs/${runId}/chat`);
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  const oldTerminal = await page.locator('.xterm').elementHandle();
  const projectA = page.locator(`.wb-project-entry[href="/runs/${runId}/chat"]`);
  const projectB = page.locator(`.wb-project-entry[href="/runs/${secondRun}/chat"]`);
  delayNextRun = true;
  const requested = new Promise(resolve => { pendingRun = resolve; });
  await projectB.click(); await requested;
  await expect(page.getByText('正在打开工作台…', { exact: true })).toBeVisible();
  await projectA.click(); releaseRun();
  await expect(page.locator('.wb-session-heading h1')).toHaveText('商业界面示例');
  await page.waitForTimeout(200);
  await expect(page.locator('.wb-session-heading h1')).not.toHaveText('Review Run B');
  await projectB.click();
  await expect(page.locator('.wb-session-heading h1')).toHaveText('Review Run B');
  assert.equal(await oldTerminal.evaluate(element => element.isConnected), false, 'leaving A must detach its terminal');
  await expect(page.locator('.xterm')).toHaveCount(0); // B explicitly has no terminal capability.
  await page.getByRole('navigation', { name: '会话功能' }).getByRole('link', { name: '文件', exact: true }).click();
  await page.getByRole('treeitem', { name: 'B.txt', exact: true }).click();
  await expect(page.getByRole('region', { name: '文件阅读' })).toContainText('Only Run B content');
  assert.ok(secondRequests.includes(secondPrefix + '/workspace/file'));
  await page.locator('.wb-navigation-utilities').getByRole('link', { name: '设置', exact: true }).click();
  await expect(page.locator('.wb-home')).toBeVisible();
  await projectA.click();
  await expect(page.locator('.wb-session-heading h1')).toHaveText('商业界面示例');
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-ready', 'true');
  assert.equal(fixture.state.input.length, inputCount, 'cross-Run and application navigation must not send input');
  await page.screenshot({ path: path.join(artifact.path, 'returned-run-a.png') });
  result.checks.push('synthetic A/B SPA navigation rejects late B metadata; B file requests stay scoped; A terminal detaches and reopens without input; application settings round-trip');
  assert.deepEqual(result.pageErrors, []);
  assert.deepEqual(result.externalRequests, []);
  success = true;
} catch (error) {
  result.failure = { stage, message: String(error.message).slice(0, 1800) };
} finally {
  await browser?.close();
  await fixture.close();
  await server?.close();
  fs.rmSync(runtime, { recursive: true, force: true });
  result.status = success ? 'passed' : 'failed';
  fs.writeFileSync(path.join(artifact.path, 'result.json'), JSON.stringify(result, null, 2) + '\n');
  artifact.finish(success);
  console.log(JSON.stringify({ artifact: artifact.path, ...result }, null, 2));
  if (!success) process.exitCode = 1;
}
