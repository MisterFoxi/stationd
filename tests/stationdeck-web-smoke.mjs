import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
import { chromium } from 'playwright';
import { PNG } from 'pngjs';

const base = process.env.STATIONDECK_URL || 'http://deck:8080';
const output = process.env.STATIONDECK_QA_OUTPUT || '/tmp/stationdeck-web-qa';
await mkdir(output, { recursive: true });
const browser = await chromium.launch({
  headless: true,
  args: ['--no-sandbox', '--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
});
const context = await browser.newContext({ viewport: { width: 1120, height: 760 }, deviceScaleFactor: 1 });
context.setDefaultTimeout(15000);
const page = await context.newPage();
const errors = [];
const requests = [];
page.on('pageerror', error => errors.push(String(error)));
page.on('console', event => { if (event.type() === 'error') errors.push(event.text()); });
page.on('request', request => requests.push(request.url()));

async function ready(target) {
  await target.waitForFunction(() => document.querySelector('#stationdeck[data-ready="true"]') || document.querySelector('#loading[data-error="true"]'));
  assert.equal(await target.locator('#loading').getAttribute('data-error'), null, 'WebAssembly startup failed');
  await target.locator('#stationdeck').focus();
  await target.waitForTimeout(300);
}

async function capture(target, name) {
  const buffer = await target.screenshot({ path: `${output}/${name}.png` });
  const png = PNG.sync.read(buffer);
  assert(png.width >= 640 && png.height >= 480);
  assert(buffer.length > 5000, 'blank canvas');
  return png;
}

function brightness(png, x, y) {
  const offset = (y * png.width + x) * 4;
  return (png.data[offset] + png.data[offset + 1] + png.data[offset + 2]) / 3;
}

try {
  const response = await page.goto(base);
  assert.equal(response.status(), 200);
  assert.equal(await page.title(), 'StationDeck');
  await ready(page);
  assert(brightness(await capture(page, 'web-home'), 700, 20) < 80, 'default dark theme did not apply');
  const wasmResponse = await page.request.get(`${base}/pkg/stationdeck_bg.wasm`);
  assert.equal(wasmResponse.status(), 200);
  assert.match(wasmResponse.headers()['content-type'], /application\/wasm/);
  assert.equal((await page.request.get(`${base}/stationd.toml`)).status(), 404);
  await page.setViewportSize({ width: 760, height: 560 });
  await page.waitForTimeout(300);
  await capture(page, 'web-compact');
  await page.keyboard.press('Control+,');
  await page.waitForTimeout(200);
  await capture(page, 'web-settings-dark');
  // Fixture at default zoom, 760x560: click the visible Light selector.
  await page.mouse.click(247, 190);
  await page.waitForTimeout(200);
  const light = await capture(page, 'web-settings-light');
  assert(brightness(light, 700, 20) > 200, 'light theme did not apply');
  await page.waitForFunction(() => Object.values(localStorage).some(value => value.includes('"theme":"Light"')));
  await page.reload();
  await ready(page);
  assert(brightness(await capture(page, 'web-reloaded'), 700, 20) > 200, 'theme not restored');
  await page.keyboard.press('Control+,');
  await page.waitForTimeout(200);
  await page.keyboard.press('Control+Home');
  await page.waitForTimeout(200);
  await capture(page, 'web-home-return');
  assert.equal(await page.locator('#stationdeck').evaluate(canvas => canvas.clientWidth), 760);
  assert.equal(await page.locator('#stationdeck').evaluate(canvas => canvas.clientHeight), 560);
  assert(requests.every(url => url.startsWith(new URL(base).origin) || url.startsWith('data:')), requests.join('\n'));
  assert.deepEqual(errors, [], 'browser errors');

  // Exercise the test module first, then enable and use the gRPC provider.
  await page.setViewportSize({ width: 1120, height: 760 });
  await page.keyboard.press('Control+,');
  await page.waitForTimeout(150);
  await page.mouse.click(210, 190);
  await page.waitForTimeout(150);
  await page.keyboard.press('Control+Home');
  await page.mouse.click(60, 278);
  await page.waitForTimeout(150);
  await capture(page, 'test-station');
  await page.mouse.click(60, 319);
  await page.waitForTimeout(100);
  await page.mouse.click(235, 175);
  await page.waitForTimeout(100);
  await capture(page, 'test-task-pending');
  await page.keyboard.press('Control+Home');
  await page.mouse.click(60, 319);
  await page.waitForTimeout(900);
  await capture(page, 'test-task-completed');
  await page.mouse.click(193, 140);
  await page.waitForTimeout(150);
  await capture(page, 'test-read-only');
  await page.mouse.click(45, 154);
  await page.waitForTimeout(150);
  await page.mouse.click(193, 156);
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).enabled_modules.length === 0);
  await page.keyboard.press('Control+Home');
  await page.waitForTimeout(100);
  await capture(page, 'no-modules');
  await page.mouse.click(45, 154);
  await page.waitForTimeout(100);
  await page.mouse.click(193, 156);
  await page.mouse.click(193, 231);
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).enabled_modules.includes('grpc'));
  await capture(page, 'modules-enabled');

  const token = (await readFile(process.env.STATIONDECK_QA_TOKEN_FILE, 'utf8')).trim();
  const fixture = process.env.STATIONDECK_FIXTURE_URL;
  assert(token.length >= 32, 'test credential is missing');
  const metrics = async () => (await (await page.request.get(`${fixture}/metrics`)).json()).requests;
  await page.request.post(`${fixture}/ready`);
  const beforeAuth = await metrics();
  assert.equal((await page.request.get(`${base}/api/station/status`)).status(), 401);
  assert.equal((await page.request.get(`${base}/api/station/status`, { headers: { Authorization: 'Bearer invalid-fixture-token' } })).status(), 401);
  assert.equal((await page.request.post(`${base}/api/station/status`, { headers: { Authorization: `Bearer ${token}` } })).status(), 405);
  assert.equal((await page.request.get(`${base}/api/station/quit`, { headers: { Authorization: `Bearer ${token}` } })).status(), 404);
  assert.equal(await metrics(), beforeAuth, 'unauthorized requests reached gRPC');

  await page.mouse.click(60, 278);
  await page.waitForTimeout(150);
  await page.mouse.click(300, 177);
  await page.waitForTimeout(100);
  await page.keyboard.insertText(token);
  await page.waitForTimeout(100);
  const statusReply = page.waitForResponse(response => response.url().endsWith('/api/station/status'));
  await page.mouse.click(235, 212);
  const connected = await (await statusReply).json();
  assert.equal(connected.state, 'connected');
  assert.equal(connected.status.station_name, 'Fixture station');
  assert.equal(connected.status.pid, 42);
  assert((await metrics()) > beforeAuth, 'the browser did not reach the real gRPC fixture');
  await page.waitForTimeout(200);
  await capture(page, 'grpc-connected');
  assert(!(await page.evaluate(() => JSON.stringify(localStorage))).includes(token), 'credentials must not be persisted');

  await page.request.post(`${fixture}/fail`);
  const failedReply = page.waitForResponse(response => response.url().endsWith('/api/station/status'));
  await page.mouse.click(235, 212);
  const failed = await (await failedReply).json();
  assert.equal(failed.state, 'unavailable');
  assert(!failed.message.includes('fixture-only'), 'upstream diagnostics were exposed');
  await page.waitForTimeout(150);
  await capture(page, 'grpc-unavailable');

  await page.request.post(`${fixture}/delay`);
  const pending = page.waitForRequest(request => request.url().endsWith('/api/station/status'));
  await page.mouse.click(235, 212);
  await pending;
  await page.mouse.click(45, 154);
  await page.waitForTimeout(100);
  await page.mouse.click(193, 231);
  await page.waitForTimeout(2300);
  await page.mouse.click(193, 231);
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).enabled_modules.includes('grpc'));
  await page.waitForTimeout(150);
  await page.mouse.click(60, 278);
  await page.waitForTimeout(150);
  const cleared = await capture(page, 'grpc-reenabled-empty');
  assert(brightness(cleared, 300, 177) <= 10, 'the connection input is not visible');
  let credentialPixels = 0;
  for (let y = 170; y < 185; y++) for (let x = 300; x < 450; x++) {
    if (brightness(cleared, x, y) > 100) credentialPixels++;
  }
  assert(credentialPixels < 5, 'the cleared password field still shows credential characters');
  const countAfterCancellation = await metrics();
  await page.mouse.click(235, 212);
  await page.waitForTimeout(150);
  assert.equal(await metrics(), countAfterCancellation, 'disabled module retained its access token');
  assert(!(await page.evaluate(() => JSON.stringify(localStorage))).includes(token), 'credentials must not be persisted after cancellation');
  await page.request.post(`${fixture}/ready`);
  assert.deepEqual(errors, [], 'unexpected browser errors during module tests');
  const broken = await context.newPage();
  await broken.route('**/*_bg.wasm', route => route.abort());
  await broken.goto(base);
  await broken.locator('#loading[data-error="true"]').waitFor();
  assert(await broken.locator('#loading-message').isVisible(), 'startup failure must be explained');
  await broken.screenshot({ path: `${output}/web-startup-error.png` });
  await broken.close();
  await page.close();
  console.log('PASS: browser WASM/WebGL, HTTP/MIME, resize, navigation, theme persistence/reload, same-origin assets, startup error, test module, authenticated real gRPC, upstream failure, cancellation and credential cleanup');
} finally {
  await context.close();
  await browser.close();
}
