// Shared only by local acceptance probes. Own Chrome through the public
// BrowserServer API so a hung close can kill its complete process group.
const { chromium: nativeChromium } = require('playwright');
let launching, server, closing;

async function launch(options) {
  if (launching || closing) throw new Error('browser probe already started or closing');
  launching = nativeChromium.launchServer({
    ...options,
    host: '127.0.0.1',
    port: 0,
    timeout: 10000,
    handleSIGINT: false,
    handleSIGTERM: false,
    handleSIGHUP: false,
  }).then(value => { server = value; return value; });
  await launching;
  if (closing) throw new Error('browser probe closing during startup');
  // The capability endpoint stays in memory; it never goes to argv or logs.
  return nativeChromium.connect(server.wsEndpoint(), { timeout: 10000 });
}

function close(code) {
  if (closing) return closing;
  closing = (async () => {
    // EOF/signal may arrive before launch resolves. Do not exit Node while
    // Playwright is still creating a browser that has no assigned handle.
    await launching?.catch(() => {});
    if (server) {
      let timer;
      const ended = await Promise.race([
        server.close().then(() => true, () => false),
        new Promise(resolve => { timer = setTimeout(() => resolve(false), 3000); }),
      ]);
      clearTimeout(timer);
      if (!ended) await server.kill();
    }
    process.exit(code);
  })().catch(() => {
    // Never print a Playwright error: it may contain a private pairing URL.
    process.exit(1);
  });
  return closing;
}

process.stdin.resume();
process.stdin.on('end', () => close(1));
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(signal, () => close(1));
process.stdout.on('error', () => close(1));

module.exports = { chromium: { launch }, close };
