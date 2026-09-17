/**
 * Real browser end-to-end tests: drive Astra with Puppeteer.
 *
 * Astra must already be running and reachable at ASTRA_HTTP (default
 * http://127.0.0.1:9222).  The test page (tests/e2e/site) must be served at
 * TEST_URL (default http://127.0.0.1:8123/index.html).
 *
 *   node tests/e2e/puppeteer/test.mjs
 */
import puppeteer from 'puppeteer-core';
import fs from 'node:fs';
import assert from 'node:assert/strict';

const ASTRA_HTTP = process.env.ASTRA_HTTP || 'http://127.0.0.1:9222';
const TEST_URL = process.env.TEST_URL || 'http://127.0.0.1:8123/index.html';
const OUT_DIR = process.env.OUT_DIR || '/tmp/astra-e2e';

let passed = 0;
const failures = [];

async function check(label, fn) {
  try {
    await fn();
    passed++;
    console.log(`  ok   ${label}`);
  } catch (err) {
    failures.push(label);
    console.log(`  FAIL ${label}: ${err.message}`);
  }
}

async function getWsEndpoint() {
  const res = await fetch(`${ASTRA_HTTP}/json/version`);
  assert.equal(res.status, 200, '/json/version status');
  const info = await res.json();
  assert.ok(info.webSocketDebuggerUrl, 'webSocketDebuggerUrl present');
  assert.ok(info.Browser.includes('Astra'), `browser string: ${info.Browser}`);
  return info.webSocketDebuggerUrl;
}

async function main() {
  fs.mkdirSync(OUT_DIR, { recursive: true });
  console.log(`astra + puppeteer end-to-end test\n  astra: ${ASTRA_HTTP}\n  page : ${TEST_URL}`);
  const ws = await getWsEndpoint();

  const browser = await puppeteer.connect({
    browserWSEndpoint: ws,
    defaultViewport: { width: 1280, height: 800 },
  });
  console.log(`  connected, ${await browser.version()}`);

  const page = await browser.newPage();
  const consoleErrors = [];
  page.on('pageerror', (e) => consoleErrors.push(String(e)));

  await check('page.goto + load event', async () => {
    const res = await page.goto(TEST_URL, { waitUntil: 'load', timeout: 30000 });
    assert.ok(res.ok(), `status ${res.status()}`);
    const title = await page.title();
    assert.equal(title, 'Astra test page');
  });

  await check('images are decoded and painted (chromium parity)', async () => {
    const info = await page.evaluate(() =>
      Array.from(document.images).map((i) => ({
        src: i.currentSrc || i.src,
        w: i.naturalWidth,
        h: i.naturalHeight,
        complete: i.complete,
      })));
    assert.ok(info.length >= 3, `expected >= 3 images, got ${info.length}`);
    for (const i of info) {
      assert.ok(i.complete, `image not loaded: ${i.src}`);
      assert.ok(i.w > 0 && i.h > 0, `image has no pixels: ${i.src} (${i.w}x${i.h})`);
    }
    const decoded = await page.evaluate(async () => {
      const img = document.getElementById('img1');
      await img.decode();
      return img.naturalWidth;
    });
    assert.ok(decoded > 0, 'img.decode() failed');
  });

  await check('video element plays (currentTime advances)', async () => {
    if (!process.env.HAS_VIDEO) {
      console.log('    (skipped: no video asset)');
      return;
    }
    const started = await page.evaluate(async () => {
      const v = document.getElementById('vid');
      if (!v) return { ok: false, reason: 'no element' };
      v.muted = true;
      v.currentTime = 0;
      try {
        await v.play();
      } catch (e) {
        return { ok: false, reason: 'play rejected: ' + e.message };
      }
      return { ok: true };
    });
    assert.ok(started.ok, JSON.stringify(started));
    const t0 = await page.evaluate(() => document.getElementById('vid').currentTime);
    await new Promise((r) => setTimeout(r, 2000));
    const t1 = await page.evaluate(() => document.getElementById('vid').currentTime);
    assert.ok(t1 > t0, `video did not advance: ${t0} -> ${t1}`);
    assert.ok(t1 > 0.3, `video time too small: ${t1}`);
    const dims = await page.evaluate(() => {
      const v = document.getElementById('vid');
      return { w: v.videoWidth, h: v.videoHeight, dur: v.duration };
    });
    assert.ok(dims.w > 0 && dims.h > 0, `no video frames decoded: ${JSON.stringify(dims)}`);
  });

  await check('javascript execution / Runtime.evaluate', async () => {
    const v = await page.evaluate(() => 6 * 7);
    assert.equal(v, 42);
    const sum = await page.$eval('#sum', (el) => el.textContent);
    assert.ok(sum.startsWith('sum=499999500000'), `unexpected #sum: ${sum}`);
  });

  await check('screenshot through astra produces a real png', async () => {
    const file = `${OUT_DIR}/puppeteer.png`;
    const buf = await page.screenshot({ path: file });
    assert.ok(buf.length > 5000, `screenshot too small: ${buf.length}`);
    assert.equal(buf[0], 0x89, 'not a png');
    assert.equal(buf.toString('latin1', 1, 4), 'PNG');
  });

  await check('rendering smoothness probe (rAF frame pacing)', async () => {
    const fps = await page.evaluate(() => new Promise((resolve) => {
      let frames = 0;
      const start = performance.now();
      function tick() {
        frames++;
        if (performance.now() - start < 1000) requestAnimationFrame(tick);
        else resolve(frames * 1000 / (performance.now() - start));
      }
      requestAnimationFrame(tick);
    }));
    console.log(`    measured ${fps.toFixed(1)} fps (software rasterizer in CI)`);
    assert.ok(fps > 5, `implausibly low frame rate: ${fps}`);
  });

  await check('astra stats exposed over CDP', async () => {
    const client = await page.createCDPSession();
    const stats = await client.send('Astra.getStats');
    assert.ok(stats.requests > 0, `no requests seen: ${JSON.stringify(stats)}`);
    assert.ok(stats.bytesOriginal > 0, 'no bytes counted');
    assert.ok(stats.bytesDelivered > 0, 'nothing delivered');
    console.log(`    ${stats.requests} requests, original ${stats.bytesOriginal} B, ` +
                `delivered ${stats.bytesDelivered} B, saved ${stats.savingPct.toFixed(1)}%`);
    const ver = await client.send('Astra.getVersion');
    assert.ok(ver.version, 'no version');
  });

  await check('cache hit on second navigation', async () => {
    const client = await page.createCDPSession();
    await client.send('Astra.resetStats');
    await page.reload({ waitUntil: 'load' });
    const stats = await client.send('Astra.getStats');
    console.log(`    after reload: cacheHits=${stats.cacheHits}, ` +
                `delivered=${stats.bytesDelivered} B, saved ${stats.savingPct.toFixed(1)}%`);
    assert.ok(stats.bytesDelivered < stats.bytesOriginal,
              `expected delivered < original after caching: ${stats.bytesDelivered} vs ${stats.bytesOriginal}`);
  });

  await check('no uncaught page errors', () => {
    assert.deepEqual(consoleErrors, []);
  });

  await check('multiple pages / targets', async () => {
    const p2 = await browser.newPage();
    await p2.goto('about:blank');
    const v = await p2.evaluate(() => 1 + 1);
    assert.equal(v, 2);
    await p2.close();
  });

  await page.close();
  await browser.disconnect();

  console.log(`\n${passed + failures.length} checks, ${failures.length} failed`);
  if (failures.length) {
    for (const f of failures) console.log(`  failed: ${f}`);
    process.exit(1);
  }
  process.exit(0); // the CDP socket can keep the event loop alive otherwise
}

main().catch((e) => {
  console.error('fatal:', e);
  process.exit(1);
});
