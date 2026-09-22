// No external requests, credentials, product page or persistent browser profile.
const { chromium, close } = require('./browser-lifecycle.cjs');
const { writeFileSync } = require('node:fs');
let browser;

(async () => {
  browser = await chromium.launch({ executablePath: process.env.WORKBENCH_TEST_CHROME, headless: true });
  const page = await browser.newPage();
  await page.goto('about:blank');
  writeFileSync(process.env.PROBE_READY_FILE, 'ready');
})().catch(() => close(1));
