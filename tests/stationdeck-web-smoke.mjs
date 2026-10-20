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
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
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
  // Read the same rectangle across completed frames. Keep the Playwright predicate
  // synchronous: an async predicate can yield a Promise handle before its result.
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const handle = await target.waitForFunction(id =>
      document.getElementById('stationdeck')?.getAttribute(`data-qa-${id}`) || false, id);
    const first = await handle.jsonValue();
    await handle.dispose();
    await animationFrame(target);
    await animationFrame(target);
    await animationFrame(target);
    if (typeof first !== 'string' || await target.locator('#stationdeck').getAttribute(`data-qa-${id}`) !== first) continue;
    const rect = first.split(',').map(Number);
    assert(rect.length === 4 && rect.every(Number.isFinite), `Invalid geometry: ${id}`);
    const canvas = await target.locator('#stationdeck').boundingBox();
    return { x: canvas.x + rect[0], y: canvas.y + rect[1], width: rect[2], height: rect[3] };
  }
  throw new Error(`Unstable canvas control: ${id}`);
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
async function openView(target, id) {
  const module = id.split('.')[0];
  await clickControl(target, `plugin-row-${module}`);
  await controlRect(target, `window-${module}`);
  if (await target.locator('#stationdeck').getAttribute(`data-qa-tab-${id}`)) {
    await clickControl(target, `tab-${id}`);
  }
}
async function setToken(kind, value) {
  await clickControl(page, 'nav-tokens');
  await clickControl(page, `tokens-${kind}`);
  await page.keyboard.press('Control+A');
  await page.keyboard.insertText(value);
  await page.waitForFunction(kind => {
    const saved = JSON.parse(localStorage.getItem('stationdeck.tokens.v1') || 'null');
    return !!saved?.[kind === 'gateway' ? 'deck_gateway' : kind === 'icecast-control' ? 'icecast_control' : 'configuration_author'];
  }, kind);
}
async function waitState(id, state) {
  await page.waitForFunction(({ id, state }) =>
    document.getElementById('stationdeck')?.getAttribute(`data-qa-state-${id}`) === state, { id, state });
}
async function waitHealth(id, tone) {
  await page.waitForFunction(({ id, tone }) =>
    document.getElementById('stationdeck')?.getAttribute(`data-qa-health-${id}`) === tone, { id, tone });
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
  assert(brightness(await capture(page, 'web-home'), 500, 5) < 80, 'default dark theme did not apply');
  await waitHealth('demo', 'good');
  await waitHealth('grpc', 'inactive');
  await controlRect(page, 'plugin-row-grpc');
  // Plugins coexist. Closing a window keeps its module alive; the launcher reopens it.
  await controlRect(page, 'window-demo');
  await controlRect(page, 'window-overview');
  const demoWindow = await controlRect(page, 'window-demo');
  await page.mouse.click(demoWindow.x + demoWindow.width - 15, demoWindow.y + 14);
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).windows.demo.open === false);
  await waitState('demo', 'Running');
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-window-demo'));
  await openView(page, 'demo.tasks');
  await controlRect(page, 'demo-run');
  await clickControl(page, 'pin-demo');
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).windows.demo.pinned);
  await page.reload();
  await ready(page);
  await controlRect(page, 'demo-run');
  assert.equal(await page.evaluate(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).windows.demo.view), 'demo.tasks');
  await clickControl(page, 'pin-demo');
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
  assert(brightness(light, 500, 5) > 200, 'light theme did not apply');
  await page.waitForFunction(() => Object.values(localStorage).some(value => value.includes('"theme":"Light"')));
  await page.reload();
  await ready(page);
  assert(brightness(await capture(page, 'web-reloaded'), 500, 5) > 200, 'theme not restored');
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
  await openView(page, 'demo.overview');
  await capture(page, 'test-station');
  await openView(page, 'demo.tasks');
  await clickControl(page, 'demo-run');
  await capture(page, 'test-task-pending');
  await page.keyboard.press('Control+Home');
  await openView(page, 'demo.tasks');
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
  // Logs is a host window, usable with no plugin enabled, and remains independent.
  await clickControl(page, 'nav-logs');
  await controlRect(page, 'window-logs');
  await controlRect(page, 'logs-view');
  await page.waitForFunction(() => Number(document.getElementById('stationdeck').getAttribute('data-qa-logs-count')) > 0);
  await capture(page, 'logs-no-modules');
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).logs_open);
  await page.reload();
  await ready(page);
  await controlRect(page, 'window-logs');
  await page.waitForFunction(() => Number(document.getElementById('stationdeck').getAttribute('data-qa-logs-count')) > 0);
  const logsWindow = await controlRect(page, 'window-logs');
  assert(logsWindow.x + logsWindow.width <= page.viewportSize().width, 'Logs window exceeds the viewport');
  await page.mouse.click(logsWindow.x + logsWindow.width - 15, logsWindow.y + 14);
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-window-logs'));
  await page.waitForFunction(() => JSON.parse(localStorage.getItem('stationdeck.settings.v1')).logs_open === false);
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
  await waitHealth('grpc', 'inactive');
  await waitHealth('overview', 'inactive');
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

  await setToken('gateway', `  ${token}  `);
  await setToken('configuration', 'author-token-fixture');
  await capture(page, 'tokens-saved-masked');
  await page.reload();
  await ready(page);
  assert((await page.evaluate(() => localStorage.getItem('stationdeck.tokens.v1'))).includes(token), 'saved gateway token was not restored');
  assert.equal(await metrics(), beforeAuth, 'reload must not automatically reconnect');
  await clickControl(page, 'nav-grpc.connection');
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-grpc-token'), null, 'credentials leaked into a plugin window');
  const statusReply = page.waitForResponse(response => response.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
  const connected = await (await statusReply).json();
  assert.equal(connected.state, 'connected');
  assert.equal(connected.status.station_name, 'Fixture station');
  assert.equal(connected.status.pid, 42);
  assert((await metrics()) > beforeAuth, 'the browser did not reach the real gRPC fixture');
  await page.waitForTimeout(200);
  await waitHealth('grpc', 'good');
  await waitHealth('overview', 'good');
  await capture(page, 'grpc-connected');
  assert(!(await page.evaluate(() => localStorage.getItem('stationdeck.settings.v1'))).includes(token), 'tokens belong in their dedicated store, not layout preferences');

  await page.request.post(`${fixture}/fail`);
  const failedReply = page.waitForResponse(response => response.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
  const failed = await (await failedReply).json();
  assert.equal(failed.state, 'unavailable');
  assert(!failed.message.includes('fixture-only'), 'upstream diagnostics were exposed');
  await page.waitForTimeout(150);
  await waitHealth('grpc', 'error');
  await waitHealth('overview', 'error');
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
  await capture(page, 'grpc-reenabled-saved');
  await waitHealth('grpc', 'inactive');
  const countAfterCancellation = await metrics();
  await animationFrame(page);
  assert.equal(await metrics(), countAfterCancellation, 're-enabling must not automatically reconnect');
  await page.request.post(`${fixture}/ready`);
  const reconnected = page.waitForResponse(r => r.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
  assert.equal((await (await reconnected).json()).state, 'connected', 'saved credentials were not reused');
  await page.request.post(`${fixture}/delay`);
  const pendingForget = page.waitForRequest(r => r.url().endsWith('/api/station/status'));
  await clickControl(page, 'grpc-refresh');
  await pendingForget;
  await clickControl(page, 'nav-tokens');
  await clickControl(page, 'forget-gateway');
  await waitHealth('grpc', 'inactive');
  await page.waitForTimeout(2300);
  await waitHealth('overview', 'inactive');
  assert(!(await page.evaluate(() => localStorage.getItem('stationdeck.tokens.v1') || '')).includes(token), 'Forget kept the gateway token');
  assert((await page.evaluate(() => localStorage.getItem('stationdeck.tokens.v1') || '')).includes('author-token-fixture'), 'gateway deletion affected configuration credentials');
  await setToken('gateway', token);
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
  await waitHealth('icecastd', 'warning');
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd', 'icecast-gateway');
  await waitState('icecastd', 'Running');
  await clickControl(page, 'nav-icecast-gateway.connection');

  const beforeIcecast = await metrics();
  assert.equal((await page.request.get(`${base}/api/icecast/streams`)).status(), 401);
  assert.equal((await page.request.post(`${base}/api/icecast/streams`, { headers: { Authorization: `Bearer ${token}` } })).status(), 405);
  assert.equal((await page.request.get(`${base}/api/services`)).status(), 401);
  const inventory = await (await page.request.get(`${base}/api/services`, {
    headers: { Authorization: `Bearer ${token}` },
  })).json();
  assert(inventory.services.some(service => service.instance === 'fixture-1'), 'signed fixture was not discovered');
  assert(inventory.bindings.some(binding => binding.interface === 'stationdeck.icecast.streams'
    && binding.instance === 'fixture-1' && binding.code === 'discovered'), 'Icecast used no discovered binding');
  assert(!JSON.stringify(inventory).includes(token), 'inventory exposed a credential');
  assert(!JSON.stringify(inventory).includes('/service-credentials'), 'inventory exposed a credential path');
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
  await waitHealth('icecast-gateway', 'good');
  await waitHealth('icecastd', 'warning');
  await page.waitForFunction(() => document.getElementById('stationdeck').getAttribute('data-qa-vital-listeners') === '7');
  // Vital information remains visible when its plugin window is hidden.
  const streamsWindow = await controlRect(page, 'window-icecastd');
  await page.mouse.click(streamsWindow.x + streamsWindow.width - 15, streamsWindow.y + 14);
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-window-icecastd'));
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-vital-listeners'), '7');
  await clickControl(page, 'nav-icecastd.streams');
  await capture(page, 'icecast-plugin-streams');
  // Real management RPCs against the isolated Icecast fixture.
  const controlToken = (await readFile('/secrets/icecast-control-token', 'utf8')).trim();
  const controlMetrics = async () => (await (await page.request.get(`${fixture}/metrics`)).json()).icecast_commands;
  const beforeControl = await controlMetrics();
  for (const headers of [{}, {Authorization:`Bearer ${token}`}]) {
    assert.equal((await page.request.post(`${base}/api/icecast/control`, {headers, data:{action:'route'}})).status(),403);
  }
  assert.equal(await controlMetrics(),beforeControl,'read credentials reached management RPCs');
  const invalidTarget = await page.request.post(`${base}/api/icecast/control`, {headers:{Authorization:`Bearer ${controlToken}`},data:{action:'metadata',mount:'/not-configured',title:'Rejected'}});
  assert.equal(invalidTarget.status(),502);
  assert(!(await invalidTarget.text()).includes('fixture-only'),'management leaked upstream diagnostics');
  await setToken('icecast-control',controlToken);
  await clickControl(page,'nav-icecastd.streams');
  let manageWindow=await controlRect(page,'window-icecastd');
  await page.mouse.move(manageWindow.x+manageWindow.width/2,manageWindow.y+14);
  await page.mouse.down();await page.mouse.move(manageWindow.x+manageWindow.width/2,140,{steps:12});await page.mouse.up();
  await animationFrame(page);await animationFrame(page);
  manageWindow=await controlRect(page,'window-icecastd');
  await page.mouse.move(manageWindow.x+manageWindow.width-4,manageWindow.y+manageWindow.height-4);
  await page.mouse.down();await page.mouse.move(manageWindow.x+manageWindow.width-4,950,{steps:12});await page.mouse.up();
  await clickControl(page,'icecast-tab-management');
  async function clickDeskControl(id) {
    for (let attempt=0;attempt<8;attempt++) {
      const rect=await controlRect(page,id);
      const window=await controlRect(page,'window-icecastd');
      const top=window.y+48;
      const bottom=Math.min(window.y+window.height-18,page.viewportSize().height-35);
      const center=rect.y+rect.height/2;
      if(center>=top&&center<=bottom) return clickControl(page,id);
      await page.mouse.move(window.x+window.width-24,Math.max(top+30,bottom-35));
      await page.mouse.wheel(0,center<top?-400:400);
      await page.waitForTimeout(150);
    }
    throw new Error(`Cannot scroll Icecast control into view: ${id}`);
  }
  async function operation(id) {
    await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-routing-controls-ready')==='true');
    const reply=page.waitForResponse(r=>r.url().endsWith('/api/icecast/control') && (id==='migration-refresh' ? r.request().postDataJSON().action==='migrations' : r.request().postDataJSON().action!=='migrations'));
    await clickDeskControl(id);
    const result=await (await reply).json();
    assert.equal(result.state,'completed');return result.result;
  }
  await clickDeskControl('icecast-program-tools');
  const route=await operation('icecast-get-route');assert.equal(route.route.input,'a');
  assert.equal(route.route.input_streams[0].mount,'/live');
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-icecast-source-a-state')==='ON AIR');
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-icecast-source-b-state'),'Ready');
  await capture(page,'icecast-routing-desk-a');
  assert.equal((await operation('icecast-take-b')).route.input,'b');
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-icecast-output-input')==='b');
  await capture(page,'icecast-routing-desk-b');
  await clickDeskControl('icecast-edit-title');
  await clickDeskControl('icecast-title');await page.keyboard.press('Control+A');await page.keyboard.insertText('Managed fixture title');
  assert.equal((await operation('icecast-set-title')).kind,'done');
  // Cohort planning, explicit confirmation, backend journal reload and selective return.
  await operation('migration-refresh');
  await page.waitForTimeout(400);
  await clickDeskControl('migration-destination');
  await clickDeskControl('migration-destination-_test-b.mp3');
  assert.equal((await operation('migration-plan')).operation.state,'planned');
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-migration-1-state')==='planned');
  const beforeMigrationCancel=await controlMetrics();
  await clickDeskControl('migration-run-1');await clickDeskControl('migration-dismiss');
  assert.equal(await controlMetrics(),beforeMigrationCancel,'dismissing migration submitted a command');
  await clickDeskControl('migration-run-1');
  assert.equal((await operation('migration-confirm')).operation.state,'away');
  await operation('migration-refresh');
  await page.waitForTimeout(400);
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-migration-1-state')==='away');
  await capture(page,'icecast-listener-maintenance');
  await clickDeskControl('migration-return-1');
  assert.equal((await operation('migration-confirm')).operation.returned,1);
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-migration-1-state')==='complete');
  assert.equal((await operation('icecast-listeners')).listeners[0].id,42);
  const beforeCancel=await controlMetrics();
  await clickDeskControl('icecast-kick-listener-42');await clickDeskControl('icecast-cancel');
  assert.equal(await controlMetrics(),beforeCancel,'cancel submitted a disconnect');
  await clickDeskControl('icecast-kick-listener-42');await operation('icecast-confirm');
  assert.equal((await operation('icecast-listeners')).listeners.length,0);
  await capture(page,'icecast-management');
  await clickDeskControl('icecast-advanced');
  await clickDeskControl('icecast-kick-source');await operation('icecast-confirm');
  await clickDeskControl('icecast-tab-streams');
  await page.waitForFunction(()=>document.getElementById('stationdeck').getAttribute('data-qa-icecast-stream-0-connected')==='false');
  const managed=await (await page.request.get(`${base}/api/icecast/streams`,{headers:{Authorization:`Bearer ${token}`}})).json();
  assert.equal(managed.status.streams[0].title,'Managed fixture title');
  await clickControl(page,'nav-tokens');await clickControl(page,'forget-icecast-control');
  await page.waitForFunction(()=>!JSON.parse(localStorage.getItem('stationdeck.tokens.v1')).icecast_control);
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-vital-listeners'),'0','forgetting control credentials interrupted supervision');
  await clickControl(page,'nav-icecastd.streams');
  assert((await page.evaluate(() => localStorage.getItem('stationdeck.tokens.v1'))).includes(token), 'shared saved token was lost');
  await page.request.post(`${fixture}/fail`);
  const icecastFailure = page.waitForResponse(r => r.url().endsWith('/api/icecast/streams'));
  await clickControl(page, 'icecast-streams-refresh');
  const unavailableIcecast = await (await icecastFailure).json();
  assert.equal(unavailableIcecast.state, 'unavailable');
  assert(!unavailableIcecast.message.includes('fixture-only'), 'Icecast upstream diagnostics were exposed');
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-icecast-stream-0-mount'));
  await waitHealth('icecastd', 'error');
  await page.waitForFunction(() => !document.getElementById('stationdeck').hasAttribute('data-qa-vital-listeners'));
  await capture(page, 'icecast-plugin-unavailable');
  await clickControl(page, 'nav-modules');
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd');
  await waitState('icecastd', 'Blocked');
  await waitHealth('icecastd', 'warning');
  assert.equal(await page.locator('#stationdeck').getAttribute('data-qa-nav-icecastd.streams'), null);
  await clickControl(page, 'module-icecast-gateway');
  await waitModules('icecastd', 'icecast-gateway');
  await waitState('icecastd', 'Running');
  await clickControl(page, 'nav-icecast-gateway.connection');
  const icecastCount = (await (await page.request.get(`${fixture}/metrics`)).json()).icecast_requests;
  await animationFrame(page);
  assert.equal((await (await page.request.get(`${fixture}/metrics`)).json()).icecast_requests, icecastCount, 're-enabling must not automatically reconnect');
  await page.request.post(`${fixture}/ready`);
  const icecastReconnected = page.waitForResponse(r => r.url().endsWith('/api/icecast/streams'));
  await clickControl(page, 'icecast-connect');
  assert.equal((await (await icecastReconnected).json()).state, 'connected');
  await clickControl(page, 'nav-tokens');
  await clickControl(page, 'forget-gateway');
  await waitHealth('icecast-gateway', 'inactive');
  await waitHealth('icecastd', 'inactive');
  await clickControl(page, 'forget-configuration');
  await page.waitForFunction(() => localStorage.getItem('stationdeck.tokens.v1') === null);
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
  console.log('PASS: browser WASM/WebGL, HTTP/MIME, resize, navigation, theme persistence/reload, same-origin assets, startup error, module dependency blocking, provider binding, test module, authenticated real gRPC, upstream failure, cancellation, credential cleanup, autonomous Icecast plugin/service injection and session Logs window');
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
