const { expect } = require('playwright/test');
const { chromium, close } = require('./browser-lifecycle.cjs');
const assert = require('node:assert/strict');
let browser, page, stage = 'launch';
const report = value => process.stdout.write(`${JSON.stringify(value)}\n`);

const screenshot = async suffix => {
  const prefix = process.env.WORKBENCH_PROBE_SCREENSHOT;
  if (!prefix) return;
  await page.screenshot({ path: `${prefix}.${suffix}.png`, fullPage: true });
  if (suffix === 'file' || suffix === 'git-conversation') {
    const panel = await page.locator('.wb-workspace-panel').boundingBox();
    await page.screenshot({ path: `${prefix}.${suffix}-detail.png`, clip: { x: 0, y: panel.y, width: panel.x + panel.width, height: Math.min(600, panel.height) } });
  }
};
setTimeout(() => { report({ stage: 'failed', check: stage, reason: 'deadline' }); close(1); }, 50000).unref();
(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } }); page = await context.newPage(); page.setDefaultTimeout(6000);
  let errors = 0, frames = 0, sockets = 0;
  page.on('pageerror', () => errors++); page.on('websocket', socket => { if (!socket.url().includes('/terminal')) return; sockets++; socket.on('framesent', frame => { if (JSON.parse(frame.payload).command?.type === 'input') frames++; }); });
  stage = 'navigate'; await page.goto(process.env.WORKBENCH_PROBE_URL);
  stage = 'terminal-ready';
  await expect(page.locator('.wb-terminal')).toHaveAttribute('data-owned', 'true'); stage = 'conversation-ready'; await expect(page.locator('.wb-model-message')).toHaveCount(1);
  const terminal = await page.locator('.xterm-helper-textarea').elementHandle(); const before = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json());
  stage = 'initial-input'; await page.locator('.xterm-helper-textarea').focus(); await page.keyboard.type('R4draft'); await expect.poll(() => frames).toBeGreaterThan(0); const initialFrames = frames;
  stage = 'files';
  await page.getByRole('button', { name: '文件', exact: true }).click();
  await expect(page.locator('.wb-workspace-panel')).toContainText('README.md'); await expect(page.locator('.wb-workspace-panel')).not.toContainText('.env');
  await expect(page.locator('.wb-message-list')).toBeVisible();
  await page.getByRole('treeitem', { name: 'src', exact: true }).click();
  await page.getByRole('treeitem', { name: 'components', exact: true }).click();
  await page.getByRole('treeitem', { name: 'config', exact: true }).click();
  const sourceFile = page.locator('[role="treeitem"][title="src/hello.ts"]');
  await sourceFile.click(); await expect(sourceFile).toHaveAttribute('aria-selected', 'true');
  await expect(page.locator('[role="tree"] .wb-read-stamp')).toHaveCount(0);
  await expect(page.getByRole('region', { name: '代码内容', exact: true })).toContainText('working');
  await expect(page.locator('.wb-file-reading img')).toHaveCount(0);
  await page.getByLabel('跳转行号').fill('3'); await page.getByRole('button', { name: '跳转', exact: true }).click(); await expect(page.locator('.wb-code-line.target')).toHaveAttribute('data-line', '3');
  await screenshot('file');
  const fileReading = await require('./file-reading-probe.cjs')(page, value => { stage = value; }, screenshot);
  report({ stage: 'progress', fileReading });
  await page.getByRole('button', { name: '关闭文件阅读', exact: true }).click(); await expect(page.locator('.wb-message-list')).toBeVisible();
  stage = 'search';
  await page.getByRole('button', { name: '搜索代码', exact: true }).click(); await page.getByRole('textbox', { name: '搜索内容' }).fill('needle');
  await page.getByRole('button', { name: '搜索', exact: true }).click(); await expect(page.locator('.wb-search-hit')).toHaveCount(2);
  await page.locator('.wb-search-hit').last().click(); await expect(page.locator('.wb-code-line.target')).toHaveAttribute('data-line', '2');
  await screenshot('search');
  stage = 'git'; await page.getByRole('button', { name: '关闭文件阅读' }).click(); await page.getByRole('button', { name: 'Git', exact: true }).click();
  const stagedSource = page.getByRole('button', { name: '查看已暂存差异 src/hello.ts（修改）', exact: true });
  await expect(stagedSource).toBeVisible(); await expect(page.locator('.wb-message-list')).toBeVisible();
  await expect(page.locator('.wb-git-name').filter({ hasText: /^hello.ts$/ })).toHaveCount(3);
  await expect(page.locator('.wb-git-mark').filter({ hasText: '重命名' })).toBeVisible();
  await expect(page.locator('.wb-git-mark').filter({ hasText: '删除' })).toBeVisible();
  await screenshot('git-conversation');
  await stagedSource.click(); await expect(stagedSource).toHaveAttribute('aria-current', 'true');
  await expect(page.getByRole('region', { name: 'Git 内容差异' })).toContainText('+export const mode = \'staged\'');
  await page.getByRole('button', { name: '未暂存', exact: true }).click(); await expect(page.getByRole('region', { name: 'Git 内容差异' })).toContainText('+export const mode = \'working\'');
  await expect(page.getByRole('button', { name: '查看未暂存差异 src/hello.ts（修改）', exact: true })).toHaveAttribute('aria-current', 'true');
  await expect(stagedSource).not.toHaveAttribute('aria-current', 'true');
  await screenshot('git-diff');
  await page.getByRole('button', { name: '打开当前文件第 2 行', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('.cm-line.target')).toHaveAttribute('data-line', '2');
  await page.getByRole('button', { name: '未暂存', exact: true }).click();
  await expect(page.getByRole('region', { name: 'Git 内容差异' })).toBeVisible();
  await page.getByRole('button', { name: '提交记录', exact: true }).click(); await expect(page.locator('.wb-git-commit')).toContainText('Add synthetic workspace');
  await screenshot('git-log');
  await page.getByRole('button', { name: '变更', exact: true }).click();
  stage = 'responsive';
  for (const width of [1024, 736, 320]) {
    await page.setViewportSize({ width, height: 900 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    if (width <= 900) { await screenshot(`git-width-${width}`); await page.getByRole('button', { name: '收起项目面板' }).click(); await expect(page.locator('.wb-file-reading')).toBeVisible(); }
    await screenshot(`width-${width}`);
    if (width <= 900) { await page.getByRole('button', { name: '文件', exact: true }).click(); await expect(page.locator('.wb-workspace-panel')).toBeVisible(); await screenshot(`files-width-${width}`); await sourceFile.click(); await expect(page.locator('.wb-workspace-panel')).toBeHidden(); await expect(page.locator('.wb-file-reading')).toBeVisible(); await page.getByRole('button', { name: 'Git', exact: true }).click(); }
  }
  assert.equal(frames, initialFrames, 'reading never submits terminal input'); assert.equal(sockets, 1, 'same terminal connection');
  assert.ok(await terminal.evaluate(e => e.isConnected)); const after = await page.evaluate(async () => (await fetch('/workbench/v1/run')).json()); assert.equal(after.processId, before.processId);assert.equal(after.runEpoch, before.runEpoch);
  stage = 'terminal-after'; await page.getByRole('button', { name: '收起项目面板' }).click();
  await page.locator('.xterm-helper-textarea').focus(); await page.keyboard.type('R4stillrunning'); await expect.poll(() => frames).toBeGreaterThan(initialFrames);
  assert.equal(errors, 0); report({ stage: 'complete', chrome: browser.version(), fileReading, errors, terminalSockets: sockets, checks: ['files', 'line-jump', 'search', 'staged-working', 'log', '1024', '736', '320', 'same-process'] }); await close(0);
})().catch(async error => { await screenshot('failure').catch(() => {}); report({ stage: 'failed', check: stage, reason: error.message }); close(1); });
