// System-Chrome regression for the probe's document ownership, not product code.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { once } = require('node:events');
const { test } = require('node:test');
const { chromium } = require('playwright');
const { WebSocketServer } = require('ws');
const { observeSecurePage, trackDocumentSockets } = require('./secure-probe-utils.cjs');

test('socket tracking survives SPA navigation but advances ownership on reload', { timeout: 20000 }, async () => {
  const temporaryRoot = await fs.mkdtemp(path.join(os.tmpdir(), 'secure-probe-lifecycle-'));
  const userDirectory = path.join(temporaryRoot, 'user-home');
  const nativeDirectory = path.join(temporaryRoot, 'native-home');
  const server = http.createServer((_, response) => {
    response.writeHead(200, { 'Content-Type': 'text/html' });
    response.end('<!doctype html><script>window.socket = new WebSocket("ws://" + location.host)</script>');
  });
  const sockets = new WebSocketServer({ server });
  let browser;
  try {
    await Promise.all([fs.mkdir(userDirectory), fs.mkdir(nativeDirectory)]);
    server.listen(0, '127.0.0.1'); await once(server, 'listening');
    browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true, env: { ...process.env, HOME: userDirectory, USERPROFILE: userDirectory, CODEX_HOME: nativeDirectory } });
    const page = await browser.newPage();
    const tracker = await trackDocumentSockets(page);
    await observeSecurePage(page, () => {});
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.waitForFunction(() => socket.readyState === WebSocket.OPEN);
    assert.equal(tracker.size, 1);
    const generation = tracker.generation;
    await page.evaluate(() => { history.pushState({}, '', '/spa'); location.hash = 'synthetic'; });
    assert.equal(tracker.generation, generation, 'SPA must retain the same document owner');
    assert.equal(tracker.size, 1, 'SPA must not clear live sockets');
    await page.reload();
    await page.waitForFunction(() => socket.readyState === WebSocket.OPEN);
    assert.equal(tracker.generation, generation + 1, 'reload must create a new document owner');
    assert.equal(tracker.size, 1, 'only the current document socket remains tracked');
    assert.equal(await page.evaluate(() => __secureProbe.socketCounts.live), 1);
    await page.evaluate(() => socket.close());
    await page.waitForFunction(() => __secureProbe.socketCounts.live === 0);
    assert.equal(tracker.size, 0);
    assert.equal(await page.evaluate(() => __secureProbe.socketCounts.closed), 1);
  } finally {
    await browser?.close();
    for (const socket of sockets.clients) socket.terminate();
    await new Promise(resolve => sockets.close(resolve));
    await new Promise(resolve => server.close(resolve));
    await fs.rm(temporaryRoot, { recursive: true, force: true });
  }
});
