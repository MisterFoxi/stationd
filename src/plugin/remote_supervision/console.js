'use strict';
const id=document.body.dataset.stationId,base=`/api/stations/${encodeURIComponent(id)}`;
const message=document.getElementById('message');
const host=document.getElementById('terminal'),connection=document.getElementById('connection-state'),focus=document.getElementById('focus-state'),application=document.getElementById('application-state');
const focusHelp=document.getElementById('focus-help'),activity=document.getElementById('input-activity');
let context,socket,terminal,fit,observer,ended=false,monitor,attempts=0;
let lastTransport=0,lastTui=0,sequence=0;
let terminalFocused=false,windowActive=true;
const pending=new Map();
function badge(element,label,ok=false){if(element.textContent!==label)element.textContent=label;element.dataset.level=ok?'ok':'warning';}
function textareaFocused(){const input=terminal?.textarea;return !!input&&input.getRootNode().activeElement===input;}
function updateFocus(){
  // A real textarea focus/key event is stronger evidence than document.hasFocus().
  const focused=!!terminal&&!terminal.options.disableStdin&&terminalFocused&&windowActive&&document.visibilityState!=='hidden';
  badge(focus,focused?t('FOCUSED'):t('TERMINAL NOT FOCUSED'),focused);focusHelp.hidden=focused;
}
function focusTerminal(){
  if(!terminal||terminal.options.disableStdin)return;
  terminal.focus();terminalFocused=textareaFocused();
  if(terminalFocused)windowActive=true;
  updateFocus();
}
function requestTerminalFocus(){
  const target=terminal,current=socket;
  if(!target||target.options.disableStdin)return;
  focusTerminal();
  // Mouse handlers/default actions can move focus again after the initial focus().
  // Retry after that gesture, but never steal it from a subsequent user action.
  const settled=document.activeElement;
  requestAnimationFrame(()=>{
    if(terminal!==target||socket!==current||target.options.disableStdin||!windowActive||document.visibilityState==='hidden')return;
    const active=document.activeElement;
    if(active!==settled&&active!==document.body&&!host.contains(active))return;
    focusTerminal();
  });
}
function updateStatus(){
  updateFocus();
  const now=performance.now(),online=socket?.readyState===WebSocket.OPEN;
  const inputDelayed=online&&pending.size&&now-pending.values().next().value>3000;
  const indication=inputDelayed?t('INPUT DELAYED'):'';
  if(activity.textContent!==indication)activity.textContent=indication;
  if(!online){badge(application,t('UNKNOWN'));return;}
  const transportFresh=now-lastTransport<6000;
  badge(connection,transportFresh?t('CONNECTED'):t('CONNECTION UNRESPONSIVE'),transportFresh);
  const responsive=lastTui>0&&now-lastTui<5000;
  badge(application,responsive&&transportFresh?t('READY'):lastTui?t('BUSY'):t('UNKNOWN'),responsive&&transportFresh);

  if(!transportFresh&&now-lastTransport>10000){message.textContent=t('Le serveur ne répond plus. Rechargez la page pour réessayer.');socket.close();}
}
async function request(url,body){const response=await fetch(url,{method:body===undefined?'GET':'POST',credentials:'same-origin',headers:body===undefined?{}:{'Content-Type':'application/json','Accept-Language':webminLanguage,'X-CSRF-Token':context.csrf_token},...(body===undefined?{}:{body:JSON.stringify(body)})});if(!response.ok){const error=new Error(response.status===503?t('Quota atteint ou serveur occupé. Réessayez dans quelques secondes.'):response.status===404?t('La console est désactivée sur ce serveur.'):t('Accès refusé ou session expirée.'));error.status=response.status;throw error;}return response.json();}
function send(data){if(socket?.readyState!==WebSocket.OPEN)return false;try{socket.send(JSON.stringify(data));return true;}catch{return false;}}
function dimensions(){const proposed=fit.proposeDimensions();return{cols:Math.max(20,Math.min(300,proposed?.cols||80)),rows:Math.max(5,Math.min(120,proposed?.rows||24))};}
function resize(){if(!terminal)return;const size=dimensions();terminal.resize(size.cols,size.rows);send({type:'resize',...size});}
function cleanup(){terminalFocused=false;clearInterval(monitor);monitor=null;observer?.disconnect();pending.clear();activity.textContent='';if(terminal)terminal.options.disableStdin=true;updateFocus();}
function stop(){const previous=socket;socket=null;previous?.close();cleanup();badge(connection,t('DISCONNECTED'));badge(application,t('UNKNOWN'));updateFocus();}
async function open(){
  stop();message.textContent='';ended=false;
  badge(connection,attempts++?t('RECONNECTING'):t('CONNECTING'));lastTui=0;sequence=0;
  terminal?.dispose();terminal=new Terminal({fontSize:14,scrollback:1000,allowProposedApi:false,disableStdin:true,theme:{background:'#101822'}});
  fit=new FitAddon.FitAddon();terminal.loadAddon(fit);terminal.open(host);resize();
  const target=terminal;
  terminal.textarea.addEventListener('focus',()=>{if(terminal!==target)return;terminalFocused=true;windowActive=true;updateFocus();});
  terminal.textarea.addEventListener('blur',()=>{if(terminal!==target)return;terminalFocused=false;updateFocus();});
  // onKey proves actual keyboard routing; onData also includes automatic terminal replies.
  terminal.onKey(()=>{if(terminal!==target)return;terminalFocused=true;windowActive=true;updateFocus();});
  // OSC is consumed by xterm, even across fragmented PTY frames, without changing the screen.
  terminal.parser.registerOscHandler(777,data=>{
    if(terminal!==target)return true;
    const match=/^stationd;([0-9]{1,20})$/.exec(data);if(!match)return false;
    lastTui=performance.now();updateStatus();return true;
  });
  try{
    const reservation=await request(`${base}/console`,dimensions());
    const url=new URL(`${base}/console/${encodeURIComponent(reservation.id)}/ws`,location.href);url.protocol='wss:';
    const current=new WebSocket(url);socket=current;current.binaryType='arraybuffer';
    current.onopen=()=>{if(socket!==current){current.close();return;}lastTransport=performance.now();terminal.options.disableStdin=false;focusTerminal();message.textContent='';resize();send({type:'probe'});monitor=setInterval(()=>{send({type:'probe'});updateStatus();},1000);updateStatus();};
    current.onmessage=event=>{
      if(socket!==current)return;lastTransport=performance.now();
      if(event.data instanceof ArrayBuffer){const target=terminal;target.write(new Uint8Array(event.data),()=>{if(socket===current&&current.readyState===WebSocket.OPEN)current.send(JSON.stringify({type:'ack'}));});}
      else{try{
        const data=JSON.parse(event.data);
        if(data.type==='input_ack')pending.delete(data.seq);
        if(data.type==='ended'){ended=true;const reasons={idle:t('Console fermée après inactivité.'),duration:t('Durée maximale atteinte.'),launch_failed:t('Le programme stationd-tui ne peut pas démarrer. Vérifiez son installation.'),revoked:t('Session ou autorisation révoquée.'),revoked_or_stopped:t('Session révoquée ou serveur arrêté.'),slow_client:t('Console fermée : connexion trop lente.'),io_error:t('Erreur de transmission au PTY.'),rate_limited:t('Console fermée : trop de données envoyées.'),exited:t('La TUI est terminée.')};message.textContent=reasons[data.reason]||t('Console fermée.');stop();}
      }catch{current.close();}}
      updateStatus();
    };
    current.onerror=()=>{if(socket===current&&!ended)message.textContent=t('Connexion à la console impossible.');};
    current.onclose=()=>{if(socket===current){socket=null;cleanup();badge(connection,t('DISCONNECTED'));badge(application,t('UNKNOWN'));updateFocus();if(!ended)message.textContent=t('Console déconnectée. Rechargez la page pour réessayer.');}};
    terminal.onData(data=>{
      const points=[...data];
      for(let offset=0;offset<points.length;offset+=512){
        if(pending.size>=256||current.bufferedAmount>65536){message.textContent=t('Envoi interrompu : connexion saturée. Les caractères restants ne sont pas envoyés.');break;}
        const seq=++sequence;
        if(!send({type:'input',seq,data:points.slice(offset,offset+512).join('')})){message.textContent=t('Entrée non envoyée : console déconnectée.');break;}
        pending.set(seq,performance.now());
      }
      updateStatus();
    });
    observer=new ResizeObserver(resize);observer.observe(host);
  }catch(error){cleanup();badge(connection,error.status===503?t('BUSY'):t('DISCONNECTED'));badge(application,t('UNKNOWN'));message.textContent=error.message;}
}
// Capture runs even if terminal selection/mouse handlers stop propagation.
host.addEventListener('pointerdown',event=>{if(event.button===0)focusTerminal();},{capture:true});
host.addEventListener('click',event=>{if(event.button===0)requestTerminalFocus();},{capture:true});
window.addEventListener('focus',()=>{windowActive=true;terminalFocused=textareaFocused();updateFocus();});
window.addEventListener('blur',()=>{windowActive=false;updateFocus();});
document.addEventListener('visibilitychange',updateFocus);
document.getElementById('logout').addEventListener('click',async()=>{try{await request('/auth/logout',{});stop();location.assign('/login');}catch(error){message.textContent=error.message;}});
window.addEventListener('pagehide',stop,{once:true});
(async()=>{try{context=await request('/api/session');const station=context.stations.find(station=>station.id===id);if(station?.role!=='admin')throw new Error(t('Cette console nécessite le rôle admin sur cette station.'));document.getElementById('station-name').textContent=t('Console — {name}',{name:station.label});document.title=t('Console — {name}',{name:station.label});document.getElementById('account').textContent=context.name;await open();}catch(error){message.textContent=error.message;}})();
