/**
 * Control experiment: connect Puppeteer straight to the engine's own DevTools
 * endpoint (no Astra in between).  Used by run_real_browser_tests.sh to tell
 * apart "Astra broke it" from "this engine/puppeteer combination behaves that
 * way anyway".
 */
import puppeteer from 'puppeteer-core';

const ws = process.env.DIRECT_WS;
if (!ws) {
  console.log('DIRECT_WS not set - skipping direct probe');
  process.exit(0);
}
console.log(`direct connect probe: ${ws}`);
const browser = await puppeteer.connect({ browserWSEndpoint: ws, protocolTimeout: 25000 });
console.log('  connected:', await browser.version());
const page = await browser.newPage();
await page.goto(process.env.TEST_URL || 'http://127.0.0.1:8123/index.html',
                { waitUntil: 'load', timeout: 25000 });
console.log('  title:', await page.title());
const imgs = await page.evaluate(() => Array.from(document.images).map(i => i.naturalWidth));
console.log('  image widths:', JSON.stringify(imgs));
await browser.disconnect();
console.log('  direct probe OK');
process.exit(0);
