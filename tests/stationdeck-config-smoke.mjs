import assert from 'node:assert/strict';
import { readFile, mkdir } from 'node:fs/promises';
import { chromium } from 'playwright';

const base = process.env.STATIONDECK_URL || 'http://deck:8080';
const phase = process.env.STATIONDECK_CONFIG_PHASE || 'seed';
const token = (await readFile('/author-secrets/editor', 'utf8')).trim();
const reader = (await readFile('/author-secrets/reader', 'utf8')).trim();
const browser = await chromium.launch({headless: true, args: ['--no-sandbox', '--use-gl=angle', '--use-angle=swiftshader', '--enable-unsafe-swiftshader']});
const context = await browser.newContext({ viewport: { width: 1120, height: 900 } });
const page = await context.newPage();
context.setDefaultTimeout(15000);
const errors = [];
page.on('pageerror', e => errors.push(String(e)));
const headers = { Authorization: `Bearer ${token}` };
const url = `${base}/api/config/targets/demo-1`;
const get = async () => { for (let attempt=0;attempt<12;attempt++) { const r = await page.request.get(url,{headers}); if ([502,503].includes(r.status())) { await new Promise(resolve=>setTimeout(resolve,500)); continue; } assert.equal(r.status(),200); return r.json(); } throw Error('Configuration API did not recover after restart.'); };
const docs = message => [{plugin:'demo.settings',instance:'default',version:1,values:{message,interval_seconds:1}}];
async function waitFor(check, label, timeout = 35000) {
  const deadline=Date.now()+timeout;
  while (Date.now()<deadline) { try { if (await check()) return; } catch (error) { if (error.code !== 'ENOENT') throw error; } await new Promise(r=>setTimeout(r,300)); }
  throw Error(`Timed out: ${label}`);
}
async function publish(documents) {
  let current=await get();
  const saved=await page.request.put(`${url}/draft`,{headers,data:{base_draft:current.draft_version,documents}});
  assert.equal(saved.status(),200);current=await saved.json();
  const published=await page.request.post(`${url}/publish`,{headers,data:{draft_version:current.draft_version,base_revision:current.published?.revision || 0}});
  assert.equal(published.status(),200);const view=await published.json();
  await waitFor(async()=> (await get()).applied_revision===view.published.revision,'gRPC agent application');
  return view;
}
try {
  if (phase==='seed') {
    assert.equal((await page.request.get(`${base}/api/config/catalog`)).status(),401);
    assert.equal((await page.request.get(`${base}/api/config/catalog`,{headers:{Authorization:`Bearer ${reader}`}})).status(),200);
    assert.equal((await page.request.post(`${url}/publish`,{headers:{Authorization:`Bearer ${reader}`},data:{draft_version:0,base_revision:0}})).status(),403);
    assert.equal((await page.request.get(`${base}/api/config/targets/foreign`,{headers})).status(),403);
    const invalid=docs('invalid');invalid[0].values.interval_seconds=0;
    assert.equal((await page.request.put(`${url}/draft`,{headers,data:{base_draft:0,documents:invalid}})).status(),422);
    const existing=await get();
    const first=existing.published ? existing : await publish(docs('Published through StationDeck'));
    assert.equal(first.published.revision,1);
    assert.equal((await page.request.put(`${url}/draft`,{headers,data:{base_draft:0,documents:docs('stale')}})).status(),409);
    assert.equal((await page.request.post(`${url}/publish`,{headers,data:{draft_version:1,base_revision:0}})).status(),409);
    const current=await get();assert.equal(current.report.code,'applied');
    await waitFor(async()=>JSON.parse(await readFile('/agent-state/runtime.json','utf8')).revision===1,'demo actually consumes configuration');
    console.log('PASS: author permissions, schema validation, conflict prevention, broadcast discovery, real gRPC pull/report and demo consumption');
  } else if (phase==='offline') {
    const min=Number(process.env.STATIONDECK_CONFIG_MIN_TIME || 0);
    await waitFor(async()=>{try { const state=JSON.parse(await readFile('/agent-state/runtime.json','utf8'));return state.observed_at>=min && state.revision>=1;}catch{return false;}},'offline cache restored after agent restart');
    console.log('PASS: restarted agent consumes its last valid configuration while the authority is stopped');
  } else if (phase==='ambiguous') {
    let current=await get();const applied=current.applied_revision;
    const documents=docs('Must wait for an unambiguous authority');
    const saved=await page.request.put(`${url}/draft`,{headers,data:{base_draft:current.draft_version,documents}});
    assert.equal(saved.status(),200);current=await saved.json();
    const publication=await page.request.post(`${url}/publish`,{headers,data:{draft_version:current.draft_version,base_revision:current.published.revision}});
    assert.equal(publication.status(),200);
    await new Promise(resolve=>setTimeout(resolve,2500));
    assert.equal((await get()).applied_revision,applied,'ambiguous discovery selected an authority');
    assert.equal(JSON.parse(await readFile('/agent-state/runtime.json','utf8')).revision,applied);
    console.log('PASS: ambiguous broadcast leaves the last applied configuration running');
  } else if (phase==='resume' || phase==='explicit') {
    const current=await get();assert(current.published.revision>=1);
    if (phase==='resume') {
      assert(current.history.length>=1,'publication history was lost on restart');
      const first=current.history[0];
      const updated=await publish(docs('Updated after authority restart'));
      const rollback=await publish(first.documents);
      assert.equal(rollback.published.revision,updated.published.revision+1);
      assert.deepEqual((await get()).history[0],first,'immutable first publication changed');
      console.log('PASS: authority restart persistence, agent reconnect and rollback as a new immutable revision');
    } else {
      const next=await publish(docs('Explicit endpoint wins over invalid discovery'));
      assert.equal((await get()).applied_revision,next.published.revision);
      console.log('PASS: explicit Docker DNS endpoint works with an invalid discovery destination');
    }
  } else if (phase==='ui') {
    await page.setViewportSize({width:1600,height:1400});
    await context.addInitScript(()=>{
      if (!localStorage.getItem('stationdeck.settings.v1')) localStorage.setItem('stationdeck.settings.v1',JSON.stringify({version:1,theme:'Dark',zoom:1,enabled_modules:['configuration']}));
    });
    async function click(id) {
      const h=await page.waitForFunction(id=>document.getElementById('stationdeck')?.getAttribute(`data-qa-${id}`)||false,id);
      const r=(await h.jsonValue()).split(',').map(Number);await h.dispose();
      await page.mouse.move(r[0]+r[2]/2,r[1]+r[3]/2);
      await page.waitForTimeout(100);
      await page.mouse.click(r[0]+r[2]/2,r[1]+r[3]/2);
      await page.waitForTimeout(100);
    }
    async function ready() { await page.waitForFunction(()=>document.querySelector('#stationdeck[data-ready="true"]'));await page.locator('#stationdeck').focus();await page.waitForTimeout(300); }
    await page.goto(base);await ready();
    await mkdir('/tmp/config-qa',{recursive:true});
    await click('nav-tokens');await click('tokens-configuration');await page.keyboard.insertText(token);
    await page.waitForFunction(()=>!!JSON.parse(localStorage.getItem('stationdeck.tokens.v1')||'null')?.configuration_author);
    await page.screenshot({path:'/tmp/config-qa/tokens-saved.png'});
    await page.reload();await ready();
    await click('nav-configuration.authoring');
    // Expand the actual plugin window so schema fields and publication controls
    // are visible; markers can also describe content clipped by its scroll area.
    const windowHandle=await page.waitForFunction(()=>document.getElementById('stationdeck')?.getAttribute('data-qa-window-configuration')||false);
    const windowRect=(await windowHandle.jsonValue()).split(',').map(Number);await windowHandle.dispose();
    await page.mouse.move(windowRect[0]+windowRect[2]-4,windowRect[1]+windowRect[3]-4);
    await page.mouse.down();await page.mouse.move(windowRect[0]+windowRect[2]-4,1250,{steps:12});await page.mouse.up();
    await page.waitForTimeout(300);
    await page.screenshot({path:'/tmp/config-qa/configuration-empty.png'});
    if (process.env.STATIONDECK_CONFIG_CAPTURE_ONLY!=='1') {
      const connected=page.waitForResponse(r=>r.url().endsWith('/api/config/catalog'));
      await click('config-connect');assert.equal((await connected).status(),200);
      await page.screenshot({path:'/tmp/config-qa/configuration-connected.png'});
      const before=await get();
      await click('config-field-0-message');await page.keyboard.press('Control+A');await page.keyboard.insertText('Edited in StationDeck');
      const saved=page.waitForResponse(r=>r.url().endsWith('/draft'));await click('config-save');assert.equal((await saved).status(),200);
      const publication=page.waitForResponse(r=>r.url().endsWith('/publish'));await click('config-publish');assert.equal((await publication).status(),200);
      await waitFor(async()=> (await get()).applied_revision===before.published.revision+1,'UI publication applied by agent');
      assert.equal((await get()).published.documents[0].values.message,'Edited in StationDeck');
      const refreshed=page.waitForResponse(r=>r.url().endsWith('/api/config/catalog'));await click('config-connect');await refreshed;
      await page.screenshot({path:'/tmp/config-qa/configuration-published.png'});
      await click('config-disconnect');
      assert((await page.evaluate(()=>localStorage.getItem('stationdeck.tokens.v1'))).includes(token),'disconnect removed the saved author token');
      await click('nav-tokens');await click('forget-configuration');
      await page.waitForFunction(()=>localStorage.getItem('stationdeck.tokens.v1')===null);
      assert(!(await page.evaluate(()=>JSON.stringify(localStorage))).includes(token),'Forget retained the author token');
      assert.deepEqual(errors,[]);
      console.log('PASS: dedicated Tokens, author token persistence/reload, schema editing, draft save, publication, agent application, disconnect and explicit Forget');
    }
  }
} finally { await context.close();await browser.close(); }