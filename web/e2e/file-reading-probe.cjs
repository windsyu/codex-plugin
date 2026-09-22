const { expect } = require('playwright/test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

module.exports = async function checkFileReading(page, stage, screenshot) {
  const session = await page.context().newCDPSession(page);
  const heap = async () => { await session.send('HeapProfiler.collectGarbage'); return (await session.send('Runtime.getHeapUsage')).usedSize; };
  const baselineHeapBytes = await heap();
  const editor = page.locator('.cm-content');
  const scroller = page.locator('.cm-scroller');
  const modifier = process.platform === 'darwin' ? 'Meta' : 'Control';
  const open = async name => {
    await page.locator(`[role="treeitem"][title="docs/${name}"]`).click();
    await expect(page.locator('.wb-file-title')).toContainText(name);
    await expect(editor).toBeVisible();
  };
  const jump = async line => {
    await page.getByLabel('跳转行号', { exact: true }).fill(String(line));
    await page.getByRole('button', { name: '跳转', exact: true }).click();
    await expect(page.locator('.cm-line.target')).toHaveAttribute('data-line', String(line));
  };
  // Exercise the browser clipboard event: CodeMirror must copy from its full
  // model even though most of the selected lines are not mounted in the DOM.
  const copied = async () => editor.evaluate(element => {
    const data = new DataTransfer();
    element.dispatchEvent(new ClipboardEvent('copy', { bubbles: true, cancelable: true, clipboardData: data }));
    return data.getData('text/plain');
  });
  await page.getByRole('treeitem', { name: 'docs', exact: true }).click();
  stage('continuous-302'); await open('continuous.txt');
  await expect(page.locator('.wb-code-navigation')).toContainText('共 302 行');
  await expect(page.getByRole('button', { name: /上一段|下一段/ })).toHaveCount(0);
  await jump(195);
  await scroller.hover(); await page.mouse.wheel(0, 500);
  await expect(page.locator('.cm-line[data-line="210"]')).toBeVisible();
  await jump(302); await expect(page.locator('.cm-line.target')).toContainText('ROW_302');
  await screenshot('continuous');
  await scroller.evaluate(el => { el.scrollTop = 0; });
  await expect(page.locator('.cm-line[data-line="1"]')).toBeVisible();
  stage('large-file'); const started = Date.now(); await open('large.txt');
  await expect(page.locator('.wb-code-navigation')).toContainText('共 30000 行');
  const openingMs = Date.now() - started;
  await jump(30000); await expect(page.locator('.cm-line.target')).toContainText('ROW_30000');
  const lineNodes = await page.locator('.cm-line').count(); assert.ok(lineNodes < 250, `bounded DOM: ${lineNodes}`);
  await page.keyboard.press(`${modifier}+a`);
  const complete = await copied();
  assert.ok(complete.startsWith('ROW_1\n')); assert.ok(complete.endsWith('ROW_30000')); assert.equal(complete.split('\n').length, 30000);
  const currentEditor = await editor.elementHandle(); const scrollTop = await scroller.evaluate(el => el.scrollTop);
  await Promise.all([
    page.waitForResponse(r => r.url().includes('/workspace/file?') && r.url().includes('large.txt')),
    page.getByRole('button', { name: '重新读取', exact: true }).click(),
  ]);
  assert.ok(await currentEditor.evaluate(el => el.isConnected));
  await expect.poll(() => scroller.evaluate(el => el.scrollTop)).toBe(scrollTop);
  await editor.focus(); assert.equal(await copied(), complete, 'unchanged refresh preserves full selection');
  await page.keyboard.type('not-editable'); assert.equal(await copied(), complete, 'typing cannot modify a read-only file');
  await currentEditor.dispose();
  stage('file-search'); await page.keyboard.press(`${modifier}+f`);
  const find = page.getByRole('textbox', { name: '文件内查找', exact: true });
  await find.fill(''); await find.pressSequentially('ROW_25001'); await find.press('Enter');
  await expect(page.locator('.cm-searchMatch-selected')).toContainText('ROW_25001');
  await expect(page.locator('.cm-line[data-line="25001"]')).toBeVisible();
  await page.getByRole('button', { name: '关闭查找' }).click();
  await jump(25001); await screenshot('large-file');
  stage('changed-file');
  fs.appendFileSync(path.join(process.env.WORKBENCH_PROBE_PROJECT, 'docs/large.txt'), '\nEXTERNAL_CHANGE');
  await page.getByRole('button', { name: '重新读取', exact: true }).click();
  await expect(page.locator('.wb-code-navigation')).toContainText('共 30001 行');
  await expect(page.locator('.cm-line[data-line="25001"]')).toBeVisible();
  await jump(30001); await expect(page.locator('.cm-line.target')).toContainText('EXTERNAL_CHANGE');
  stage('dense-file'); await open('dense.txt');
  await expect(page.locator('.wb-code-navigation')).toContainText('共 500000 行');
  await jump(500000);
  const denseLineNodes = await page.locator('.cm-line').count();
  assert.ok(denseLineNodes < 250);
  const denseHeapBytes = await heap();
  stage('long-line'); await open('long-line.txt');
  await editor.focus(); await page.keyboard.press(`${modifier}+a`);
  const long = await copied(); assert.equal(long, 'x'.repeat(20000) + 'LONG_LINE_END');
  assert.ok(await scroller.evaluate(el => el.scrollWidth > el.clientWidth * 5));
  await scroller.evaluate(el => { el.scrollLeft = el.scrollWidth; });
  await expect(page.locator('.cm-content')).toContainText('LONG_LINE_END');
  stage('file-error'); await page.locator('[role="treeitem"][title="docs/oversize.txt"]').click();
  await expect(page.locator('.wb-file-reading')).toContainText('文件超过 1 MiB');
  await expect(page.locator('.cm-editor')).toHaveCount(0);
  await open('empty.txt'); await expect(page.locator('.wb-code-navigation')).toContainText('共 1 行');
  await page.getByRole('button', { name: '关闭文件阅读' }).click();
  await expect(page.locator('.cm-editor')).toHaveCount(0);
  const closedHeapBytes = await heap();
  await session.detach();
  await page.locator('[role="treeitem"][title="src/hello.ts"]').click();
  await expect(editor).toBeVisible();
  return { baselineHeapBytes, denseHeapBytes, closedHeapBytes, denseLineNodes, denseLines: 500000, openingMs, largeFileLines: 30000, renderedLineNodes: lineNodes, fullSelectionCopy: true, unchangedRefresh: true, changedRefresh: true, longLineCharacters: long.length };
};
