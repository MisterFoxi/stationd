const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../web/player.js'), 'utf8');
function browser(compact = false) {
  const elements = new Map();
  class Element {
    constructor() { this.children=[]; this.attributes={}; this.events={}; this.assignments=0; this.dataset={}; this.value='0.8'; this.paused=true; this.volume=1; this.muted=false; this.hidden=false; this.playCalls=0; }
    append(...items) { this.children.push(...items); }
    replaceChildren(...items) { this.children=items; }
    getAttribute(key) { return this.attributes[key] ?? null; }
    setAttribute(key,value) { this.attributes[key]=String(value); }
    removeAttribute(key) { delete this.attributes[key]; }
    set src(value) { this.assignments++; this.attributes.src=value; }
    addEventListener(event, callback) { this.events[event]=callback; }
    async play() { this.playCalls++; this.paused=false; this.events.playing?.(); }
    pause() { this.paused=true; this.events.pause?.(); }
    load() { this.error=null; }
  }
  const document = {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); },
    createElement() { return new Element(); },
    body: {dataset:{},classList:{toggle(key,value){if(key==='compact')document.compact=value;}}},
    documentElement: {style:{setProperty(){}}}
  };
  let resolveFetch, stream;
  const events = {};
  const context = vm.createContext({document, location:{pathname:'/player/demo',search:compact?'?compact=1':''}, URLSearchParams, Date, Intl,
    fetch:() => new Promise(resolve => {resolveFetch=resolve;}),
    EventSource:class {constructor(){stream=this;} addEventListener(key,fn){this[key]=fn;} close(){this.closed=true;}},
    setInterval(){}, window:{addEventListener(key,fn){events[key]=fn;}}
  });
  vm.runInContext(source,context);
  return {document,elements,stream,events,resolveFetch};
}
function data(title='Now') {
  return {station:{slug:'demo',label:'Demo',description:'<img onerror=alert(1)>',stream_url:'https://radio.example.test/live.mp3',theme:{accent:'#a3e635',background:'#111827',foreground:'#f9fafb'}},
    status:'online',stale:false,air:{observed_at:Math.floor(Date.now()/1000),timezone:'Europe/Paris',state:'running',kind:'track',now_playing:{title,artist:'Artist'},coming_next:[{title:'Committed'}],indicative:[{title:'Forecast'}],agenda:[],indicative_agenda:[],history:[]}};
}
test('renders metadata as text and keeps real queue separate from forecast',() => {
  const b=browser(); b.stream.snapshot({data:JSON.stringify(data('<script>bad()</script>'))});
  assert.equal(b.elements.get('title').textContent,'<script>bad()</script>');
  assert.equal(b.elements.get('description').textContent,'<img onerror=alert(1)>');
  assert.equal(b.elements.get('next').children[0].children[0].textContent,'Committed');
  assert.equal(b.elements.get('indicative').children[0].children[0].textContent,'Forecast');
});
test('metadata updates do not restart the audio source; compact links to full player',() => {
  const b=browser(true); b.stream.snapshot({data:JSON.stringify(data())}); b.stream.snapshot({data:JSON.stringify(data('New title'))});
  assert.equal(b.elements.get('audio').assignments,1); assert.equal(b.document.compact,true);
  assert.equal(b.elements.get('full').href,'/player/demo');
  b.events.pagehide(); assert.equal(b.stream.closed,true);
});
test('late initial HTTP response cannot overwrite newer SSE metadata',async () => {
  const b=browser(); b.stream.snapshot({data:JSON.stringify(data('New'))});
  b.resolveFetch({ok:true,json:async()=>data('Old')});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(b.elements.get('title').textContent,'New');
});
test('stale snapshots and stream failure are visible',() => {
  const b=browser(); const snapshot=data(); snapshot.stale=true;
  b.stream.snapshot({data:JSON.stringify(snapshot)});
  assert.equal(b.elements.get('status').textContent,'Informations hors ligne');
  b.elements.get('audio').events.error(); assert.match(b.elements.get('audio-status').textContent,/Flux indisponible/);
});

test('custom controls play, pause, adjust volume and mute without changing source', async () => {
  const b=browser(); b.stream.snapshot({data:JSON.stringify(data())});
  const audio=b.elements.get('audio');
  assert.equal(audio.controls,false); assert.equal(b.elements.get('custom-controls').hidden,false);
  await b.elements.get('play').events.click();
  assert.equal(audio.playCalls,1); assert.equal(audio.paused,false);
  assert.equal(b.elements.get('play').attributes['aria-label'],'Mettre en pause');
  await b.elements.get('play').events.click(); assert.equal(audio.paused,true);
  b.elements.get('volume').value='0.25'; b.elements.get('volume').events.input();
  assert.equal(audio.volume,0.25);
  b.elements.get('mute').events.click(); assert.equal(audio.muted,true);
  assert.equal(b.elements.get('mute').attributes['aria-label'],'Rétablir le son');
  assert.equal(audio.assignments,1);
});

test('rejected playback is visible and keeps the listen action available', async () => {
  const b=browser(); b.elements.get('audio').play=async()=>{throw Error('unavailable');};
  await b.elements.get('play').events.click();
  assert.match(b.elements.get('audio-status').textContent,/Flux indisponible/);
  assert.equal(b.elements.get('play').attributes['aria-pressed'],'false');
});

test('artwork remains optional and failed images fall back to the typographic design', () => {
  const b=browser(); const snapshot=data();
  b.stream.snapshot({data:JSON.stringify(snapshot)}); assert.equal(b.elements.get('artwork').hidden,true);
  snapshot.station.artwork_url='https://radio.example.test/visual.jpg';
  b.stream.snapshot({data:JSON.stringify(snapshot)}); assert.equal(b.elements.get('artwork').hidden,false);
  b.elements.get('artwork').events.error();
  b.stream.snapshot({data:JSON.stringify(snapshot)}); assert.equal(b.elements.get('artwork').hidden,true);
});

test('pausing a connecting live stream releases it without showing a false failure', async () => {
  const b=browser(); const audio=b.elements.get('audio'); let rejectPlay;
  audio.play=()=>{audio.paused=false; audio.events.play(); return new Promise((_,reject)=>{rejectPlay=reject;});};
  audio.load=()=>{if(rejectPlay)rejectPlay(Object.assign(new Error('cancelled'),{name:'AbortError'}));};
  const pending=b.elements.get('play').events.click();
  assert.equal(b.elements.get('play').attributes['aria-pressed'],'true');
  await b.elements.get('play').events.click(); await pending;
  assert.equal(audio.paused,true);
  assert.doesNotMatch(b.elements.get('audio-status').textContent,/indisponible/);
  assert.equal(b.elements.get('play').attributes['aria-pressed'],'false');
});

test('history expands on demand and a sleeping station has an explicit queue message', () => {
  const b=browser(); const snapshot=data();
  snapshot.air.state='sleeping'; snapshot.air.coming_next=[]; snapshot.air.indicative=[];
  snapshot.air.history=Array.from({length:12},(_,i)=>({title:'Played '+i,artist:'Artist'}));
  b.stream.snapshot({data:JSON.stringify(snapshot)});
  assert.match(b.elements.get('next').children[0].textContent,/station est en veille/);
  assert.equal(b.elements.get('history').children.length,8);
  b.elements.get('history-more').events.click(); assert.equal(b.elements.get('history').children.length,12);
  b.stream.snapshot({data:JSON.stringify(snapshot)}); assert.equal(b.elements.get('history').children.length,12);
});
