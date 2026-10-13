const {test} = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const source = fs.readFileSync(require('node:path').join(__dirname, '../web/player.js'), 'utf8');
function browser(compact = false) {
  const elements = new Map();
  class Element {
    constructor() { this.children=[]; this.attributes={}; this.events={}; this.assignments=0; }
    append(...items) { this.children.push(...items); }
    replaceChildren(...items) { this.children=items; }
    getAttribute(key) { return this.attributes[key] ?? null; }
    set src(value) { this.assignments++; this.attributes.src=value; }
    addEventListener(event, callback) { this.events[event]=callback; }
  }
  const document = {
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); },
    createElement() { return new Element(); },
    body: {classList:{toggle(key,value){document.compact=value;}}},
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
