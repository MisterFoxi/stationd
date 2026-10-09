// Run: node tests/webmin-language.js. Browser translations and user data isolation.
'use strict';
const assert=require('node:assert/strict'),fs=require('node:fs'),vm=require('node:vm');
const root='src/plugin/remote_supervision/';
const catalog=JSON.parse(fs.readFileSync(root+'translations.json','utf8'));
const runtime='const WEBMIN_TRANSLATIONS='+JSON.stringify(catalog)+';'+fs.readFileSync(root+'i18n.js','utf8');
for(const language of ['fr','en','de']){
  const elements=new Map();
  function element(){return {textContent:'',children:[],attributes:{},classList:{toggle(){}},append(...nodes){this.children.push(...nodes);},replaceChildren(...nodes){this.children=nodes;},setAttribute(key,value){this.attributes[key]=value;},remove(){}};}
  const document={documentElement:{lang:language},body:{dataset:{page:'test'}},getElementById:id=>{if(!elements.has(id))elements.set(id,element());return elements.get(id);},createElement:element};
  const context=vm.createContext({document});
  vm.runInContext(runtime+fs.readFileSync(root+'remote.js','utf8'),context);
  const evaluate=code=>vm.runInContext(code,context);
  const translated=source=>catalog[source]?.[language]||source;
  assert.equal(evaluate('mediaText(null,"live")'),translated('Direct DJ'));
  assert.equal(evaluate('audienceText(1)'),translated('{value} auditeur').replace('{value}','1'));
  assert.equal(evaluate('audienceText(2)'),translated('{value} auditeurs').replace('{value}','2'));
  assert.equal(evaluate('states.running'),translated('En diffusion'));
  assert.equal(evaluate('notes[7]'),translated('La suite est simulée.'));
  assert.equal(evaluate('alerts.pool_empty'),translated('Aucun média diffusable'));
  assert.equal(evaluate('mediaText({title:"Se connecter",artist:"En diffusion"})'),'Se connecter — En diffusion');
  assert.equal(evaluate('t("Console — {name}",{name:"$& <Station>"})'),translated('Console — {name}').replace('{name}',()=>'$& <Station>'));
  assert.equal(evaluate('timeText(1700000000,"UTC")'),new Date(1700000000000).toLocaleTimeString(language,{timeZone:'UTC',hour:'2-digit',minute:'2-digit',second:'2-digit'}));
  evaluate('consoleEnabled=true');
  for(const role of ['admin','viewer','helper']){
    evaluate('renderNetwork('+JSON.stringify({stations:[{id:'one',label:'One',role,connection:'online',state:'running',alerts:[]}]})+')');
    assert.equal(evaluate('cards.get("one").link.href'),role==='admin'?'/station/one/console':'/station/one');
    assert.equal(evaluate('cards.get("one").element.children.filter(node=>node.href).length'),0); // No separate TUI action.
    assert.equal(evaluate('cards.get("one").element.children[0].children.length'),1); // Station name is the link.
  }
  evaluate('consoleEnabled=false');
  evaluate('renderNetwork('+JSON.stringify({stations:[{id:'one',label:'One',role:'admin',connection:'online',state:'running',alerts:[]}]})+')');
  assert.equal(evaluate('cards.get("one").link.href'),'/station/one');
  const network=stations=>evaluate('renderNetwork('+JSON.stringify({stations:stations.map((station,index)=>({id:String(index),label:'Station '+index,connection:'online',state:'running',alerts:[],...station}))})+')');
  network([{audience:2},{audience:3},{audience:0}]);
  assert.equal(elements.get('network-total').textContent,'5');
  assert.equal(elements.get('network-total-note').textContent,'');
  network([{audience:1},{audience:null}]);
  assert.equal(elements.get('network-total').textContent,'1');
  assert.equal(elements.get('network-total-note').textContent,translated('Total partiel — audience indisponible pour {value} station(s).').replace('{value}','1'));
  assert.equal(evaluate('cards.get("1").audience.textContent'),translated('Audience inconnue'));
  network([{audience:null}]);
  assert.equal(elements.get('network-total').textContent,'—');
  assert.equal(evaluate('cards.size'),1);
  network([{audience:0}]);
  assert.equal(elements.get('network-total').textContent,'0');
  assert.equal(elements.get('network-total-note').textContent,'');
  network([]);
  assert.equal(elements.get('network-total').textContent,'0');
  assert.equal(evaluate('cards.size'),0);
  assert.equal(elements.get('message').textContent,translated('Aucune station accessible.'));
  for(const [source,entry]of Object.entries(catalog)){
    for(const target of ['en','de'])assert.ok(entry[target],source+' '+target);
    assert.deepEqual((entry[language]||source).match(/\{\{?[a-zA-Z_]+\}?\}/g),(source).match(/\{\{?[a-zA-Z_]+\}?\}/g),source);
  }
}
for(const page of ['login','enroll','network','station','console'])assert.ok(fs.readFileSync(root+page+'.html','utf8').includes('<script src="/i18n.js" defer></script>'));
console.log('Webmin language: fr/en/de texts, placeholders, dates, singular/plural, user data and all page scripts passed.');
