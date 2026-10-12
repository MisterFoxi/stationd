// Run: node tests/webmin-dj.js. Readiness and private-monitor UI behavior.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const elements=new Map(),listeners={},requests=[];
function element(id){if(!elements.has(id))elements.set(id,{textContent:'',dataset:{},children:[],hidden:false,disabled:false,value:'',listeners:{},
  addEventListener(name,fn){this.listeners[name]=fn;},setAttribute(){},append(...nodes){this.children.push(...nodes);},replaceChildren(...nodes){this.children=nodes;this.renderCount=(this.renderCount||0)+1;},
  pause(){this.paused=true;},removeAttribute(name){delete this[name];},load(){},async play(){this.paused=false;}});return elements.get(id);}
const root='src/plugin/remote_supervision/';
const translations=JSON.parse(fs.readFileSync(root+'translations.json','utf8'));
const state={dj:'marc',enabled:true,host:'dj.example.test',port:8005,mount:'/live',state:'ready',can_take:true,can_preview:true,level_db:-20,peak_db:-9,clipping:false,other_dj:'',buffer_seconds:5,manual_takeover:true};
const context={document:{documentElement:{lang:'fr'},body:{dataset:{stationId:'one'}},getElementById:element,createElement:()=>element('node-'+Math.random())},window:{addEventListener:(name,fn)=>listeners[name]=fn},location:{assign:url=>requests.push(url)},navigator:{clipboard:{writeText:async value=>requests.push(value)}},AbortController,setTimeout:()=>1,clearTimeout(){},
  fetch:async(url,options)=>{requests.push([url,options]);return{ok:true,json:async()=>url==='/api/session'?{csrf_token:'csrf',name:'Alice',stations:[{id:'one',label:'One'}]}:state};}};
vm.createContext(context);
vm.runInContext('const WEBMIN_TRANSLATIONS='+JSON.stringify(translations)+';'+fs.readFileSync(root+'i18n.js','utf8')+fs.readFileSync(root+'dj.js','utf8'),context);
const run=code=>vm.runInContext(code,context);
(async()=>{
  for(let i=0;i<10;i++)await Promise.resolve();
  run('render('+JSON.stringify(state)+')');
  assert.equal(element('take').disabled,false);assert.equal(element('preview').disabled,false);
  assert.match(element('signal-state').textContent,/Votre flux est bon/);
  const count=element('encoder-settings').renderCount;
  run('render('+JSON.stringify(state)+')');
  assert.equal(element('encoder-settings').renderCount,count,'polling must preserve focus on copy buttons');
  await element('preview').listeners.click();
  assert.equal(element('preview-audio').src,'/api/stations/one/dj/preview');
  assert.equal(element('preview-stop').disabled,false);
  run('unavailable({status:503})');
  assert.equal(element('take').disabled,true);assert.equal(element('preview').disabled,true);
  assert.equal(element('preview-audio').src,undefined);assert.equal(element('preview-audio').hidden,true);
  assert.match(element('signal-state').textContent,/indisponible/);
  run('render('+JSON.stringify({...state,state:'silent',can_take:false})+')');
  assert.equal(element('take').disabled,true);
  run('render('+JSON.stringify({...state,state:'on_air',can_take:false})+')');
  assert.match(element('signal-state').textContent,/antenne/);assert.equal(element('stop').disabled,false);
  run('render('+JSON.stringify({...state,state:'busy',other_dj:'<other>',can_take:false,can_preview:false})+')');
  assert.match(element('signal-help').textContent,/<other>/);assert.equal(element('stop').disabled,true);
  run('render('+JSON.stringify({...state,host:'',manual_takeover:false})+')');
  assert.equal(element('host-help').hidden,false);assert.equal(element('legacy').hidden,false);
  listeners.pagehide();assert.equal(element('preview-audio').src,undefined);
  assert.ok(requests.every(entry=>!Array.isArray(entry)||!entry[0].includes('password')&&!entry[0].includes('token')));
  console.log('DJ browser: ready/stale/silent/on-air/busy states, preview teardown, source settings and copy focus passed.');
})().catch(error=>{console.error(error);process.exitCode=1;});
