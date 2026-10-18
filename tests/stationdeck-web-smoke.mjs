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

// The isolated image enables ui-test. Geometry comes from actual egui Responses;
// Playwright still clicks the canvas and follows the production event path.
let lastControl = null;
async function animationFrame(target) {
  await target.evaluate(() => new Promise(resolve => requestAnimationFrame(() => resolve())));
}
async function controlRect(target, id) {
  // egui popups have a sizing/placement pass before becoming interactive. A marker
  // from that first pass is not sufficient evidence that its rectangle is final.
  await target.waitForFunction(async id => {
    const read = () => document.getElementById('stationdeck')?.getAttribute(`data-qa-${id}`);
    const first = read();
    if (!first) return false;
    for (let frame = 0; frame < 3; frame++) {
      await new Promise(resolve => requestAnimationFrame(resolve));
      if (read() !== first) return false;
    }
    return true;
  }, id);
  const rect = await target.locator('#stationdeck').evaluate((canvas, id) =>
    canvas.getAttribute(`data-qa-${id}`).split(',').map(Number), id);
  assert(rect.length === 4 && rect.every(Number.isFinite), `Invalid geometry: ${id}`);
  const canvas = await target.locator('#stationdeck').boundingBox();
  return { x: canvas.x + rect[0], y: canvas.y + rect[1], width: rect[2], height: rect[3] };
}
async function clickControl(target, id) {
  let rect = await controlRect(target, id);
  await target.mouse.move(rect.x + rect.width / 2, rect.y + rect.height / 2);
  // Let egui register the pointer in the popup's foreground layer before pressing.
  await animationFrame(target);
  await animationFrame(target);
  rect = await controlRect(target, id);
  const x = rect.x + rect.width / 2;
  const y = rect.y + rect.height / 2;
  const viewport = target.viewportSize();
  assert(x >= 0 && x < viewport.width && y >= 0 && y < viewport.height, `Control outside viewport: ${id}`);
  lastControl = { id, x, y };
  await target.mouse.move(x, y);
  await target.mouse.down();
  await animationFrame(target);
  await animationFrame(target);
  await target.mouse.up();
  await animationFrame(target);
  await animationFrame(target);
}async function waitModules(...ids) {
  await page.waitForFunction(ids => {
    const saved = JSON.parse(localStorage.getItem('stationdeck.settings.v1'));
    return saved?.enabled_modules.length === ids.length && ids.every(id => saved.enabled_modules.includes(id));
  }, ids);
}
async function waitState(id, state) {
  await page.waitForFunction(({ id, state }) =>
    document.getElementById('stationdeck')?.getAttribute(`data-qa-state-${id}`) === state, { id, state });
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
  await clickControl(page, 'theme-light');
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

  // Fresh preferences include demo AND the common overview consumer.
  await page.setViewportSize({ width: 1120, height: 1000 });
  await page.keyboard.press('Control+,');
  await clickControl(page, 'theme-dark');
  await page.keyboard.press('Control+Home');
  await clickControl(page, 'nav-demo.overview');
  await capture(page, 'test-station');
  await clickControl(page, 'nav-demo.tasks');
  await clickControl(page, 'demo-run');
  await capture(page, 'test-task-pending');
  await page.keyboard.press('Control+Home');
  await clickControl(page, 'nav-demo.tasks');
  await page.waitForTimeout(900);
  await capture(page, 'test-task-completed');
  await clickControl(page, 'demo-read-only');
  await capture(page, 'test-read-only');
  await clickControl(page, 'nav-modules');
  await clickControl(page, 'module-demo');
  await waitModules('overview');
  await waitState('overview', 'Blocked');
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-nav-overview.station'), null, 'blocked consumer kept its view');
  await capture(page, 'overview-blocked');
  await clickControl(page, 'module-overview');
  await waitModules();
  await page.keyboard.press('Control+Home');
  await capture(page, 'no-modules');
  await clickControl(page, 'nav-modules');
  await clickControl(page, 'module-demo');
  await waitModules('demo');
  await clickControl(page, 'module-grpc');
  await waitModules('demo', 'grpc');
  await clickControl(page, 'module-overview');
  await waitModules('demo', 'grpc', 'overview');
  await waitState('overview', 'Running');
  await clickControl(page, 'provider');
  await clickControl(page, 'provider-grpc');
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).overview_provider === 'grpc');
  await waitState('overview', 'Running');
  await capture(page, 'modules-enabled');
  // A disconnected provider remains locally available to the same consumer.
  await clickControl(page, 'nav-overview.station');
  await capture(page, 'overview-grpc-disconnected');
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

  await clickControl(page, 'nav-grpc.connection');
  await page.waitForTimeout(150);
  await clickControl(page, 'grpc-token');
  await page.waitForTimeout(100);
  // Terminal copy/paste may include surrounding whitespace.
  await page.keyboard.insertText(`  ${token}  `);
  await page.waitForTimeout(100);
  const statusReply = page.waitForResponse(response => response.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
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
  await clickControl(page, 'grpc-refresh');
  const failed = await (await failedReply).json();
  assert.equal(failed.state, 'unavailable');
  assert(!failed.message.includes('fixture-only'), 'upstream diagnostics were exposed');
  await page.waitForTimeout(150);
  await capture(page, 'grpc-unavailable');

  await page.request.post(`${fixture}/delay`);
  const pending = page.waitForRequest(request => request.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
  await pending;
  await clickControl(page, 'nav-modules');
  await page.waitForTimeout(100);
  await clickControl(page, 'module-grpc');
  await waitModules('demo', 'overview');
  await waitState('overview', 'Blocked');
  await page.waitForTimeout(2300);
  await clickControl(page, 'module-grpc');
  await waitModules('demo', 'grpc', 'overview');
  await waitState('overview', 'Running');
  await page.waitForTimeout(150);
  await clickControl(page, 'nav-grpc.connection');
  await page.waitForTimeout(150);
  const cleared = await capture(page, 'grpc-reenabled-empty');
  const input = await controlRect(page, 'grpc-token');
  assert(brightness(cleared, Math.floor(input.x + input.width / 2), Math.floor(input.y + input.height / 2)) <= 10, 'the connection input is not visible');
  let credentialPixels = 0;
  for (let y = Math.ceil(input.y + 3); y < input.y + input.height - 3; y++) {
    for (let x = Math.ceil(input.x + 107); x < Math.min(input.x + 257, input.x + input.width - 3); x++) {
      if (brightness(cleared, x, y) > 100) credentialPixels++;
    }
  }
  assert(credentialPixels < 5, 'the cleared password field still shows credential characters');
  const countAfterCancellation = await metrics();
  await clickControl(page, 'grpc-refresh');
  await page.waitForTimeout(150);
  assert.equal(await metrics(), countAfterCancellation, 'disabled module retained its access token');
  assert(!(await page.evaluate(() => JSON.stringify(localStorage))).includes(token), 'credentials must not be persisted after cancellation');
  await page.request.post(`${fixture}/ready`);
  assert.deepEqual(errors, [], 'unexpected browser errors during module tests');
  // Validate the autonomous Icecast plugin with no StationD module enabled.
  await clickControl(page, 'nav-modules');
  await clickControl(page, 'module-overview');
  await waitModules('demo', 'grpc');
  await clickControl(page, 'module-demo');
  await waitModules('grpc');
  await clickControl(page, 'module-grpc');
  await waitModules();
  await clickControl(page, 'module-icecastd');
  await waitModules('icecastd');
  await waitState('icecastd', 'Blocked');
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd', 'icecast-gateway');
  await waitState('icecastd', 'Running');
  await clickControl(page, 'nav-icecast-gateway.connection');
  await clickControl(page, 'icecast-token');
  await page.keyboard.insertText(`  ${token}  `);
  const beforeIcecast = await metrics();
  assert.equal((await page.request.get(`${base}/api/icecast/streams`)).status(), 401);
  assert.equal((await page.request.post(`${base}/api/icecast/streams`, { headers: { Authorization: `Bearer ${token}` } })).status(), 405);
  const icecastReply = page.waitForResponse(r => r.url().endsWith('/api/icecast/streams'));
  await clickControl(page, 'icecast-connect');
  const icecastStatus = await (await icecastReply).json();
  assert.equal(icecastStatus.state, 'connected');
  assert.equal(icecastStatus.status.server_id, 'Fixture Icecast');
  assert.equal(await metrics(), beforeIcecast, 'the Icecast plugin unexpectedly called StationD');
  await clickControl(page, 'nav-icecastd.streams');
  await page.waitForFunction(() => {
    const canvas = document.getElementById('stationdeck');
    return canvas.getAttribute('data-qa-icecast-stream-0-mount') === '/live'
      && canvas.getAttribute('data-qa-icecast-stream-0-connected') === 'true'
      && canvas.getAttribute('data-qa-icecast-stream-0-listeners') === '7'
      && canvas.getAttribute('data-qa-icecast-stream-1-connected') === 'false';
  });
  await capture(page, 'icecast-plugin-streams');
  assert(!(await page.evaluate(() => JSON.stringify(localStorage))).includes(token), 'Icecast provider persisted its credential');
  await page.request.post(`${fixture}/fail`);
  const icecastFailure = page.waitForResponse(r => r.url().endsWith('/api/icecast/streams'));
  await clickControl(page, 'icecast-streams-refresh');
  const unavailableIcecast = await (await icecastFailure).json();
  assert.equal(unavailableIcecast.state, 'unavailable');
  assert(!unavailableIcecast.message.includes('fixture-only'), 'Icecast upstream diagnostics were exposed');
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-icecast-stream-0-mount'));
  await capture(page, 'icecast-plugin-unavailable');
  await clickControl(page, 'nav-modules');
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd');
  await waitState('icecastd', 'Blocked');
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-nav-icecastd.streams'), null);
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd', 'icecast-gateway');
  await waitState('icecastd', 'Running');
  await clickControl(page, 'nav-icecast-gateway.connection');
  const icecastCount = (await (await page.request.get(`${fixture}/metrics`)).json()).icecast_requests;
  await clickControl(page, 'icecast-connect'); // Disabled after stop cleared the token.
  await animationFrame(page);
  assert.equal((await (await page.request.get(`${fixture}/metrics`)).json()).icecast_requests, icecastCount);
  assert.deepEqual(errors, [], 'browser errors in the autonomous Icecast plugin');
  await page.request.post(`${fixture}/ready`);

  const broken = await context.newPage();
  await broken.route('**/*_bg.wasm', route => route.abort());
  await broken.goto(base);
  await broken.locator('#loading[data-error="true"]').waitFor();
  assert(await broken.locator('#loading-message').isVisible(), 'startup failure must be explained');
  await broken.screenshot({ path: `${output}/web-startup-error.png` });
  await broken.close();
  await page.close();
  console.log('PASS: browser WASM/WebGL, HTTP/MIME, resize, navigation, theme persistence/reload, same-origin assets, startup error, module dependency blocking, provider binding, test module, authenticated real gRPC, upstream failure, cancellation, credential cleanup and autonomous Icecast plugin/service injection');
} catch (error) {
  await page.screenshot({ path: `${output}/web-failure.png` }).catch(() => {});
  console.error('Browser page errors:', errors);
  console.error('Last canvas interaction:', lastControl);
  // Only allowlisted local preferences and read-only QA geometry/state. No tokens.
  const snapshot = await page.locator('#stationdeck').evaluate(canvas => {
    let saved = {};
    try { saved = JSON.parse(localStorage.getItem('stationdeck.settings.v1')) || {}; } catch {}
    return {
      preferences: { enabled_modules: saved.enabled_modules, overview_provider: saved.overview_provider, theme: saved.theme },
      controls: Object.fromEntries(Array.from(canvas.attributes).filter(a => a.name.startsWith('data-qa-')).map(a => [a.name, a.value])),
    };
  }).catch(() => null);
  console.error('Canvas failure snapshot:', JSON.stringify(snapshot));
  throw error;
} finally {
  await context.close();
  await browser.close();
}
