// Measurement harness for the real reading page. Raw Chrome traces stay in
// memory; only numeric samples leave this process. No pairing URL is logged.
const { chromium, close } = require('./browser-lifecycle.cjs');
const { createInterface } = require('node:readline');
const { execFileSync } = require('node:child_process');

let browser, stage = 'launch';
const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);

setTimeout(() => { send({ stage: 'failed', check: stage }); close(1); }, 90000).unref();

(async () => {
  browser = await chromium.launch({
    executablePath: process.env.WORKBENCH_TEST_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: process.env.WORKBENCH_PROBE_HEADED !== '1',
  });
  const context = await browser.newContext({ viewport: { width: 1024, height: 740 } });
  const page = await context.newPage();
  const product = process.env.WORKBENCH_PROBE_PRODUCT_PAGE === '1';
  await page.addInitScript(() => {
    window.r5Received = [];
    const NativeEventSource = window.EventSource;
    window.EventSource = class extends NativeEventSource {
      constructor(...args) {
        super(...args);
        this.addEventListener('view', event => {
          const value = JSON.parse(event.data);
          if (window.r5Received.length < 20000) window.r5Received.push({ seq: value.viewSeq, at: performance.now() });
        });
      }
    };
  });
  let pageErrors = 0, snapshots = 0;
  page.on('pageerror', () => pageErrors++);
  page.on('request', request => { if (new URL(request.url()).pathname === '/workbench/v1/live/snapshot') snapshots++; });
  stage = 'pair';
  await page.goto(process.env.WORKBENCH_PROBE_URL);
  await page.waitForFunction(product => (product ? document.querySelector('.wb-connection') : document.getElementById('connection'))?.textContent === '已连接', product);
  await page.bringToFront();
  const cdp = await context.newCDPSession(page);
  const system = await browser.newBrowserCDPSession();
  const frame = (await cdp.send('Page.getFrameTree')).frameTree.frame.id;
  await cdp.send('Performance.enable');
  await page.evaluate(product => {
    window.r0Commits = [];
    if (product) {
      const seen = new Set();
      new MutationObserver(() => {
        const sampleIds = [];
        for (const element of document.querySelectorAll('.wb-message-body')) {
          for (const match of element.textContent.matchAll(/s(\d{5}) /g)) {
            const id = Number(match[1]); if (!seen.has(id)) { seen.add(id); sampleIds.push(id); }
          }
        }
        if (sampleIds.length && window.r0Commits.length < 20000) {
          const seq = window.r0Commits.length + 1;
          performance.mark(`r0-view-${seq}`);
          window.r0Commits.push({ seq, sampleIds, domMs: performance.now() });
        }
      }).observe(document.querySelector('.wb-message-list'), { childList: true, subtree: true, characterData: true });
      return;
    }
    new MutationObserver(records => {
      if (!records.some(record => record.attributeName === 'data-view-seq')) return;
      const seq = Number(document.body.dataset.viewSeq);
      const at = performance.now();
      if (window.r0Commits.length < 20000) {
        performance.mark(`r0-view-${seq}`);
        window.r0Commits.push({ seq, domMs: at });
      }
    }).observe(document.body, { attributes: true, attributeFilter: ['data-view-seq'] });
  }, product);
  let peakRssKiB = 0, peakHeapBytes = 0, sampling = false, resourcesAvailable = true;
  async function resources() {
    if (sampling) return;
    sampling = true;
    try {
      const processes = (await system.send('SystemInfo.getProcessInfo')).processInfo;
      const ids = processes.map(process => Number(process.id)).filter(Number.isSafeInteger);
      if (ids.length) {
        const output = execFileSync('ps', ['-o', 'rss=', '-p', ids.join(',')], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
        peakRssKiB = Math.max(peakRssKiB, output.trim().split(/\s+/).reduce((sum, value) => sum + Number(value), 0));
      }
      const metrics = (await cdp.send('Performance.getMetrics')).metrics;
      peakHeapBytes = Math.max(peakHeapBytes, metrics.find(metric => metric.name === 'JSHeapUsedSize')?.value || 0);
    } catch { resourcesAvailable = false; }
    finally { sampling = false; }
  }
  await resources();
  const cpuStart = (await system.send('SystemInfo.getProcessInfo')).processInfo.reduce((sum, process) => sum + process.cpuTime, 0);
  const sampler = setInterval(resources, 250);
  await cdp.send('Tracing.start', { categories: 'devtools.timeline,blink.user_timing', transferMode: 'ReturnAsStream' });
  const input = createInterface({ input: process.stdin });
  input.on('close', () => close(1));
  send({ stage: 'ready', browser: browser.version(), headless: process.env.WORKBENCH_PROBE_HEADED !== '1' });
  for await (const line of input) {
    const command = JSON.parse(line);
    stage = command.op;
    if (command.op === 'clock') {
      send({ stage: 'clock', browserMs: await page.evaluate(() => performance.now()) });
    } else if (command.op === 'start') {
      if (command.busyMs) await page.evaluate(ms => setTimeout(() => {
        const until = performance.now() + ms;
        while (performance.now() < until) { /* intentional slow-viewer fault */ }
      }, 300), Math.min(5000, command.busyMs));
      send({ stage: 'started' });
    } else if (command.op === 'finish') {
      await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
      const commits = await page.evaluate(() => ({ samples: window.r0Commits, received: window.r5Received, visibility: document.visibilityState, focused: document.hasFocus() }));
      const finished = new Promise(resolve => cdp.once('Tracing.tracingComplete', resolve));
      await cdp.send('Tracing.end');
      const { stream } = await finished;
      let raw = '';
      while (true) {
        const chunk = await cdp.send('IO.read', { handle: stream, size: 1024 * 1024 });
        raw += chunk.data;
        if (raw.length > 64 * 1024 * 1024) throw new Error('trace limit');
        if (chunk.eof) break;
      }
      await cdp.send('IO.close', { handle: stream });
      const events = JSON.parse(raw).traceEvents;
      const marks = new Map(events.filter(event => event.name?.startsWith('r0-view-')).map(event => [Number(event.name.slice(8)), event]));
      const paints = events.filter(event => event.name === 'Paint' && event.args?.data?.frame === frame).sort((a, b) => a.ts - b.ts);
      const samples = commits.samples.map(sample => {
        const mark = marks.get(sample.seq);
        const paint = mark && paints.find(paint => paint.pid === mark.pid && paint.ts >= mark.ts);
        return { ...sample, paintDelayMs: paint ? (paint.ts - mark.ts) / 1000 : null };
      });
      clearInterval(sampler);
      await resources();
      const cpuEnd = (await system.send('SystemInfo.getProcessInfo')).processInfo.reduce((sum, process) => sum + process.cpuTime, 0);
      send({ stage: 'result', samples, received: commits.received, paintCount: paints.length, markCount: marks.size, visibility: commits.visibility, focused: commits.focused, snapshots, pageErrors, peakRssKiB, peakHeapBytes, resourcesAvailable, cpuSeconds: cpuEnd - cpuStart });
      await close(0);
      return;
    }
  }
})().catch(() => { send({ stage: 'failed', check: stage }); close(1); });
