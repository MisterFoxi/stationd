'use strict';
const message=document.getElementById('message');
async function request(url,body,csrf) {
  const response=await fetch(url,{method:body===undefined?'GET':'POST',credentials:'same-origin',headers:body===undefined?{}:{'Content-Type':'application/json',...(csrf?{'X-CSRF-Token':csrf}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
  if(!response.ok){const error=new Error(response.status===429?'Trop de tentatives. Réessayez dans une minute.':'Accès refusé. Vérifiez vos identifiants ou demandez un nouveau lien.');error.status=response.status;throw error;}
  return response.json();
}
async function run(button,work){button.disabled=true;message.textContent='';try{await work();}catch(error){message.textContent=error.message;}finally{button.disabled=false;}}
const page=document.body.dataset.page;
if(page==='login')document.getElementById('login').addEventListener('submit',event=>{event.preventDefault();run(event.target.querySelector('button'),async()=>{await request('/auth/password/login',{name:document.getElementById('name').value,password:document.getElementById('password').value});document.getElementById('password').value='';location.assign('/');});});
if(page==='enroll'){
  const token=location.hash.slice(1);history.replaceState(null,'','/enroll');
  const form=document.getElementById('enroll'),minimum=Number(document.body.dataset.passwordMinLength),maximum=Number(document.body.dataset.passwordMaxLength);
  if(!token){message.textContent='Ce lien est incomplet. Demandez un nouveau lien à votre administrateur.';form.hidden=true;}
  form.addEventListener('submit',event=>{event.preventDefault();run(form.querySelector('button'),async()=>{const password=document.getElementById('password').value;if(password!==document.getElementById('confirm').value)throw new Error('Les deux mots de passe doivent être identiques.');const length=[...password].length;if(length<minimum||length>maximum)throw new Error(`Le mot de passe doit contenir entre ${minimum} et ${maximum} caractères.`);await request('/auth/password/enroll',{token,password});form.reset();form.hidden=true;message.textContent='Mot de passe enregistré. Vous pouvez vous connecter.';});});
}
const states={running:'En diffusion',paused:'En pause',draining:'Veille en attente',sleeping:'En veille'};
const connections={connecting:'Connexion…',online:'Station connectée',offline:'Station inaccessible'};
const alerts={station_unreachable:'StationD inaccessible',onair_unavailable:'Flux d’antenne indisponible',stale_data:'Dernières données connues — périmées',liquidsoap_error:'Erreur Liquidsoap',icecast_error:'Audience Icecast indisponible',plugin_error:'Un plugin est en échec',no_liquidsoap:'Liquidsoap non configuré',pool_empty:'Aucun média diffusable',program_error:'Erreur de calcul du programme'};
const notes={1:'La station est en pause.',2:'La station est en veille.',3:'Veille à la fin du morceau.',4:'Veille dès qu’il n’y aura plus d’auditeur.',5:'Un DJ tient l’antenne.',6:'Liquidsoap n’est pas configuré.',7:'La suite est simulée.',8:'Aucun média diffusable.',9:'Programme de secours.',10:'Relais : durée inconnue.',11:'Une durée de média est inconnue.',12:'La simulation a échoué.',13:'Un filtre de plugin a échoué.',14:'La projection du programme a échoué.',15:'L’historique est indisponible.',16:'Un rendez-vous ne pourra pas couper l’antenne.',17:'Une source prévue sera vide.',18:'Un rendez-vous n’a pas coupé l’antenne.',19:'Une source était vide.'};
function mediaText(track,kind){if(track)return[track.title||'Média sans titre',track.artist].filter(Boolean).join(' — ');return({live:'Direct DJ',relay:'Relais',fallback:'Secours',halted:'Fond d’arrêt',unknown:'Média non identifié'})[kind]||'Aucun média signalé';}
function audienceText(value){return value==null?'Audience inconnue':`${value} auditeur${value===1?'':'s'}`;}
function timeText(value,zone){if(!value)return'Inconnue';try{return new Date(value*1000).toLocaleTimeString('fr-FR',{...(zone?{timeZone:zone}:{}),hour:'2-digit',minute:'2-digit',second:'2-digit'});}catch{return new Date(value*1000).toLocaleTimeString('fr-FR');}}
function list(target,items){target.replaceChildren(...items.map(text=>{const item=document.createElement('li');item.textContent=text;return item;}));}
function descriptionList(target,entries){target.replaceChildren(...entries.flatMap(([name,value])=>{const term=document.createElement('dt'),data=document.createElement('dd');term.textContent=name;data.textContent=value;return[term,data];}));}
const cards=new Map();
function renderNetwork(data){
  const keep=new Set();
  for(const station of data.stations){
    keep.add(station.id);let card=cards.get(station.id);
    if(!card){const element=document.createElement('article');element.className='station';const title=document.createElement('h2'),link=document.createElement('a');link.href=`/station/${encodeURIComponent(station.id)}`;title.append(link);const status=document.createElement('p'),media=document.createElement('p'),audience=document.createElement('p'),observed=document.createElement('p'),warnings=document.createElement('ul');observed.className='muted';element.append(title,status,media,audience,observed,warnings);card={element,link,status,media,audience,observed,warnings};cards.set(station.id,card);document.getElementById('stations').append(element);}
    card.link.textContent=station.label;card.element.classList.toggle('stale',station.stale||station.connection==='offline');
    card.status.textContent=`${connections[station.connection]||'Connexion inconnue'} · ${station.stale?'Dernier état : ':''}${states[station.state]||'État inconnu'}`;
    card.media.textContent=mediaText(station.media,station.on_air_kind);card.audience.textContent=audienceText(station.audience);
    card.observed.textContent=station.observed_at?`Antenne observée à ${timeText(station.observed_at)}`:'Aucune donnée d’antenne reçue';
    list(card.warnings,station.alerts.map(code=>alerts[code]||code));
  }
  for(const[id,card]of cards)if(!keep.has(id)){card.element.remove();cards.delete(id);}
  message.textContent=data.stations.length?'':'Aucune station accessible.';
}
let detail;
function updateProgress(){const progress=document.getElementById('progress'),label=document.getElementById('progress-text');if(!progress)return;const track=detail?.media;if(detail?.stale||!track?.started_at||!track?.duration_ms||detail.state!=='running'){progress.hidden=true;label.textContent='';return;}const duration=track.duration_ms/1000,elapsed=Math.max(0,Math.min(duration,Date.now()/1000-track.started_at));progress.hidden=false;progress.max=duration;progress.value=elapsed;const format=s=>`${Math.floor(s/60)}:${String(Math.floor(s%60)).padStart(2,'0')}`;label.textContent=`${format(elapsed)} / ${format(duration)}`;}
function renderStation(station){
  detail=station;document.getElementById('station-name').textContent=station.label;document.title=`StationD — ${station.label}`;
  document.getElementById('station-state').textContent=`${connections[station.connection]} · ${station.stale?'Dernier état : ':''}${states[station.state]||'État inconnu'}`;
  document.getElementById('media').textContent=mediaText(station.media,station.on_air_kind);
  document.getElementById('audience').textContent=audienceText(station.audience);
  document.getElementById('observed').textContent=station.observed_at?`Antenne observée à ${timeText(station.observed_at,station.timezone)}${station.stale?' — données périmées':''}`:'Aucune donnée d’antenne reçue';
  list(document.getElementById('alerts'),station.alerts.map(code=>alerts[code]||code));
  const upcoming=[];if(station.prefetched)upcoming.push(`Préchargé (confirmé) : ${mediaText(station.prefetched)}`);for(const track of station.upcoming)upcoming.push(`${timeText(track.estimated_at,station.timezone)} · ${mediaText(track)} — simulé`);list(document.getElementById('upcoming'),upcoming.length?upcoming:['Aucune suite disponible.']);
  list(document.getElementById('notes'),station.notes.map(code=>notes[code]||`Information de programme (code ${code}).`));
  const serviceNames={online:'Connecté',offline:'Inaccessible',connecting:'Connexion…',ok:'OK',error:'Erreur',disabled:'Non configuré',unknown:'Inconnu'};
  descriptionList(document.getElementById('services'),[['StationD',serviceNames[station.services.stationd]||'Inconnu'],['Pont Liquidsoap',station.services.liquidsoap==='ok'?'Sans erreur signalée':serviceNames[station.services.liquidsoap]||'Inconnu'],['Icecast',serviceNames[station.services.icecast]||'Inconnu'],['Plugins',station.services.plugins==='ok'?'Aucun échec signalé':serviceNames[station.services.plugins]||'Inconnu']]);
  document.getElementById('playlist').textContent=station.current_playlist?`Programme courant : ${station.current_playlist.name}`:'Aucun programme courant signalé.';
  list(document.getElementById('program'),station.next_playlists.map(slot=>`${timeText(slot.from,station.timezone)} · ${slot.name}${slot.issue?' — source indisponible':''}`));updateProgress();
}
async function startDashboard(){
  let context;try{context=await request('/api/session');}catch(error){if(error.status===401||error.status===403)location.assign('/login');else message.textContent='Serveur indisponible. Rechargez la page pour réessayer.';return;}
  document.getElementById('account').textContent=context.name;
  document.getElementById('logout').addEventListener('click',event=>run(event.target,async()=>{await request('/auth/logout',{},context.csrf_token);location.assign('/login');}));
  const id=document.body.dataset.stationId,isNetwork=page==='network';
  const endpoint=isNetwork?'/api/stations':`/api/stations/${encodeURIComponent(id)}`;
  const render=isNetwork?renderNetwork:renderStation;
  try{render(await request(endpoint));}catch(error){if(error.status===401||error.status===403){location.assign('/login');return;}message.textContent='Lecture indisponible. Nouvelle tentative en cours…';}
  const source=new EventSource(isNetwork?'/api/network/events':`${endpoint}/events`);
  const connection=document.getElementById('connection');let checking=false;
  source.onopen=()=>{connection.textContent='Mise à jour en direct';connection.classList.remove('warning');};
  source.addEventListener('snapshot',event=>{try{render(JSON.parse(event.data));connection.textContent='Mise à jour en direct';connection.classList.remove('warning');}catch{connection.textContent='Données reçues illisibles.';}});
  source.addEventListener('session-ended',()=>{source.close();location.assign('/login');});
  source.onerror=()=>{connection.textContent='Connexion perdue — données affichées à vérifier. Reconnexion automatique…';connection.classList.add('warning');if(checking)return;checking=true;request('/api/session').catch(error=>{if(error.status===401||error.status===403){source.close();location.assign('/login');}}).finally(()=>{checking=false;});};
  window.addEventListener('pagehide',()=>source.close(),{once:true});
  if(!isNetwork)setInterval(updateProgress,1000);
}
if(page==='network'||page==='station')startDashboard();
