// Run: node tests/webmin-console.js. Deterministic browser/protocol transitions.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const elements=new Map();
function element(id){if(!elements.has(id))elements.set(id,{textContent:'',dataset:{},hidden:false,disabled:false,listeners:{},addEventListener(type,fn){this.listeners[type]=fn;}});return elements.get(id);}
let now=100,focused=true,interval,quota=false;
const windowListeners={},documentListeners={},frames=[];
element('terminal').contains=node=>node===document.activeElement&&node===Terminal.all.at(-1)?.textarea;
const language=process.env.WEBMIN_TEST_LANGUAGE||'en';
const document={documentElement:{lang:language},body:{dataset:{stationId:'one'}},getElementById:element,activeElement:null,visibilityState:'visible',hasFocus:()=>focused,addEventListener(type,fn){documentListeners[type]=fn;}};
class Terminal{
  constructor(options){this.options=options;this.textarea=element('textarea-'+Terminal.all.length);this.textarea.getRootNode=()=>document;this.parser={registerOscHandler:(code,handler)=>{assert.equal(code,777);this.osc=handler;}};Terminal.all.push(this);}
  static all=[];
  loadAddon(){} open(){} resize(){} dispose(){this.disposed=true;}
  focus(){document.activeElement=this.textarea;this.textarea.listeners.focus();}
  onData(fn){this.input=fn;}
  onKey(fn){this.key=fn;}
  write(bytes,fn){fn();} // Display output alone never proves a live application loop.
}
class WebSocket{
  static OPEN=1;static all=[];
  constructor(){this.readyState=0;this.bufferedAmount=0;this.sent=[];WebSocket.all.push(this);}
  send(data){this.sent.push(JSON.parse(data));}
  close(){this.readyState=3;this.onclose?.();}
  connect(){this.readyState=1;this.onopen();}
  receive(data){this.onmessage({data:JSON.stringify(data)});}
}
const context=vm.createContext({document,Terminal,WebSocket,FitAddon:{FitAddon:class{proposeDimensions(){return{cols:80,rows:24};}}},ResizeObserver:class{observe(){} disconnect(){}},window:{addEventListener(type,fn){windowListeners[type]=fn;}},location:{href:'https://example.test/console/one',search:'',pathname:'/console/one'},history:{replaceState(){}},URL,URLSearchParams,ArrayBuffer,Uint8Array,performance:{now:()=>now},requestAnimationFrame:fn=>(frames.push(fn),frames.length),setInterval:fn=>(interval=fn,1),clearInterval:()=>{interval=null;},fetch:async (url,options)=>({ok:!quota,status:quota?503:200,json:async()=>{if(url!=='/api/session')assert.equal(options.headers['Accept-Language'],language);return url==='/api/session'?{csrf_token:'csrf',name:'Admin',stations:[{id:'one',role:'admin',label:'One'}]}:{id:'reservation'};}})});
vm.runInContext('const WEBMIN_TRANSLATIONS='+fs.readFileSync('src/plugin/remote_supervision/translations.json','utf8')+';'+fs.readFileSync('src/plugin/remote_supervision/i18n.js','utf8'),context);
const translated=source=>vm.runInContext('t('+JSON.stringify(source)+')',context);
vm.runInContext(fs.readFileSync('src/plugin/remote_supervision/console.js','utf8'),context);
async function flush(){await new Promise(resolve=>setImmediate(resolve));}
(async()=>{
  await flush();await element('start').listeners.click();
  const socket=WebSocket.all.at(-1),terminal=Terminal.all.at(-1);socket.connect();
  assert.equal(element('connection-state').textContent,translated('CONNECTED'));
  assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  assert.equal(element('application-state').textContent,translated('UNKNOWN'));
  terminal.osc('stationd;0');assert.equal(element('application-state').textContent,translated('READY'));
  // Reproduce an unreliable document.hasFocus(): real focus remains authoritative.
  focused=false;interval();assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  windowListeners.blur();interval();assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));assert.equal(element('focus-help').hidden,false);
  windowListeners.focus();assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  document.activeElement=element('start');terminal.textarea.listeners.blur();interval();assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));
  // Terminal-generated replies must not claim that the user's keyboard is focused.
  terminal.input('reply');assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));
  socket.receive({type:'input_ack',seq:1});socket.sent.length=0;
  element('terminal').listeners.click({button:0});assert.equal(element('focus-state').textContent,translated('FOCUSED'));assert.equal(element('focus-help').hidden,true);
  // A post-click browser action loses focus; the next frame must recover it.
  document.activeElement=document.body;terminal.textarea.listeners.blur();
  assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));
  frames.splice(0).forEach(fn=>fn());assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  element('terminal').listeners.click({button:0});
  document.activeElement=element('close');terminal.textarea.listeners.blur();
  frames.splice(0).forEach(fn=>fn());assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED')); // Do not steal focus from another control.
  element('activate-keyboard').listeners.click();windowListeners.blur();
  frames.splice(0).forEach(fn=>fn());assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));
  windowListeners.focus();
  element('activate-keyboard').listeners.click();assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  frames.splice(0).forEach(fn=>fn());
  document.visibilityState='hidden';documentListeners.visibilitychange();assert.equal(element('focus-state').textContent,translated('TERMINAL NOT FOCUSED'));
  document.visibilityState='visible';documentListeners.visibilitychange();assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  terminal.textarea.listeners.blur();terminal.key();interval();assert.equal(element('focus-state').textContent,translated('FOCUSED'));
  focused=true;
  terminal.input('z');assert.equal(element('input-activity').textContent,translated('INPUT SENT'));
  const input=socket.sent.find(data=>data.type==='input');assert.equal(input.seq,2);assert.equal(input.data,'z');
  assert.equal(fs.readFileSync('src/plugin/remote_supervision/console.html','utf8').includes('id="input-state"'),false);
  assert.equal(fs.readFileSync('src/plugin/remote_supervision/console.html','utf8').includes('id="tui-state"'),false);
  now+=3100;socket.receive({type:'probe_ack'});interval();assert.equal(element('input-activity').textContent,translated('INPUT DELAYED'));
  socket.receive({type:'input_ack',seq:input.seq});assert.equal(element('input-activity').textContent,'');
  terminal.osc('stationd;1');assert.equal(element('application-state').textContent,translated('READY'));
  // No redraw follows this key; application readiness remains independent.
  now+=6000;socket.receive({type:'probe_ack'});interval();assert.equal(element('connection-state').textContent,translated('CONNECTED'));assert.equal(element('application-state').textContent,translated('BUSY'));
  terminal.osc('stationd;1');assert.equal(element('application-state').textContent,translated('READY'));
  now+=7000;interval();assert.equal(element('connection-state').textContent,translated('CONNECTION UNRESPONSIVE'));
  now+=4000;interval();assert.equal(element('connection-state').textContent,translated('DISCONNECTED'));assert.equal(interval,null);
  await element('start').listeners.click();assert.equal(element('connection-state').textContent,translated('RECONNECTING'));
  const second=WebSocket.all.at(-1);second.connect();
  terminal.osc('stationd;99');assert.equal(element('application-state').textContent,translated('UNKNOWN')); // Old session telemetry ignored.
  terminal.textarea.listeners.blur();assert.equal(element('focus-state').textContent,translated('FOCUSED')); // Old session focus ignored.
  now+=6000;second.receive({type:'probe_ack'});interval();assert.equal(element('application-state').textContent,translated('UNKNOWN'));
  Terminal.all.at(-1).input('x');second.receive({type:'ended',reason:'io_error'});assert.equal(element('connection-state').textContent,translated('DISCONNECTED'));assert.match(element('message').textContent,/PTY/);assert.equal(interval,null);
  assert.equal(element('activate-keyboard').disabled,true);
  quota=true;await element('start').listeners.click();assert.equal(element('connection-state').textContent,translated('BUSY'));assert.equal(element('start').disabled,false);
  console.log('Webmin console: focus events despite false document.hasFocus, explicit click, window/tab blur, actual keys versus automatic replies, PTY acknowledgements, no-op keys, heartbeat delay/recovery, transport timeout, reconnect, stale telemetry and quota passed.');
})().catch(error=>{console.error(error);process.exitCode=1;});
