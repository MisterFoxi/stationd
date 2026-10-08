'use strict';
const id=document.body.dataset.stationId,base=`/api/stations/${encodeURIComponent(id)}`;
const message=document.getElementById('message'),start=document.getElementById('start'),close=document.getElementById('close');
document.getElementById('back').href=`/station/${encodeURIComponent(id)}`;
let context,socket,terminal,fit,observer,ended=false;
async function request(url,body){const response=await fetch(url,{method:body===undefined?'GET':'POST',credentials:'same-origin',headers:body===undefined?{}:{'Content-Type':'application/json','X-CSRF-Token':context.csrf_token},...(body===undefined?{}:{body:JSON.stringify(body)})});if(!response.ok)throw new Error(response.status===503?'Quota atteint ou serveur occupé. Réessayez dans quelques secondes.':response.status===404?'La console est désactivée sur ce serveur.':'Accès refusé ou session expirée.');return response.json();}
function send(data){if(socket?.readyState===WebSocket.OPEN)socket.send(JSON.stringify(data));}
function dimensions(){const proposed=fit.proposeDimensions();return{cols:Math.max(20,Math.min(300,proposed?.cols||80)),rows:Math.max(5,Math.min(120,proposed?.rows||24))};}
function resize(){if(!terminal)return;const size=dimensions();terminal.resize(size.cols,size.rows);send({type:'resize',...size});}
function stop(){socket?.close();observer?.disconnect();socket=null;close.disabled=true;start.disabled=false;}
async function open(){
  start.disabled=true;message.textContent='Ouverture de la console…';ended=false;
  terminal?.dispose();terminal=new Terminal({fontSize:14,scrollback:1000,allowProposedApi:false,disableStdin:true,theme:{background:'#101822'}});
  fit=new FitAddon.FitAddon();terminal.loadAddon(fit);terminal.open(document.getElementById('terminal'));resize();
  try{
    const reservation=await request(`${base}/console`,dimensions());
    const url=new URL(`${base}/console/${encodeURIComponent(reservation.id)}/ws`,location.href);url.protocol='wss:';
    const current=new WebSocket(url);socket=current;current.binaryType='arraybuffer';
    current.onopen=()=>{if(socket!==current){current.close();return;}terminal.options.disableStdin=false;terminal.focus();close.disabled=false;message.textContent='Console connectée. Fermez-la après utilisation.';resize();};
    current.onmessage=event=>{if(socket!==current)return;if(event.data instanceof ArrayBuffer){terminal.write(new Uint8Array(event.data),()=>{if(current.readyState===WebSocket.OPEN)current.send(JSON.stringify({type:'ack'}));});}else{try{const data=JSON.parse(event.data);if(data.type==='ended'){ended=true;const reasons={idle:'Console fermée après inactivité.',duration:'Durée maximale atteinte.',launch_failed:'Le programme stationd-tui ne peut pas démarrer. Vérifiez son installation.',revoked:'Session ou autorisation révoquée.',revoked_or_stopped:'Session révoquée ou serveur arrêté.',slow_client:'Console fermée : connexion trop lente.',exited:'La TUI est terminée.'};message.textContent=reasons[data.reason]||'Console fermée.';}}catch{current.close();}}};
    current.onerror=()=>{if(socket===current&&!ended)message.textContent='Connexion à la console impossible.';};
    current.onclose=()=>{if(socket===current){observer?.disconnect();terminal.options.disableStdin=true;close.disabled=true;start.disabled=false;socket=null;if(!ended)message.textContent='Console déconnectée. Une nouvelle ouverture crée une nouvelle session.';}};
    terminal.onData(data=>{const points=[...data];for(let offset=0;offset<points.length;offset+=512)send({type:'input',data:points.slice(offset,offset+512).join('')});});
    observer=new ResizeObserver(resize);observer.observe(document.getElementById('terminal'));
  }catch(error){message.textContent=error.message;start.disabled=false;}
}
start.addEventListener('click',open);
close.addEventListener('click',()=>{ended=true;stop();terminal.options.disableStdin=true;message.textContent='Console fermée.';});
document.getElementById('logout').addEventListener('click',async()=>{try{await request('/auth/logout',{});stop();location.assign('/login');}catch(error){message.textContent=error.message;}});
window.addEventListener('pagehide',stop,{once:true});
(async()=>{try{context=await request('/api/session');const station=context.stations.find(station=>station.id===id);if(station?.role!=='admin')throw new Error('Cette console nécessite le rôle admin sur cette station.');document.getElementById('station-name').textContent=`Console — ${station.label}`;document.title=`Console — ${station.label}`;document.getElementById('account').textContent=context.name;start.disabled=false;message.textContent='Prête à ouvrir.';}catch(error){message.textContent=error.message;}})();
