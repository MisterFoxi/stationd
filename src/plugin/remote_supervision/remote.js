'use strict';
const message=document.getElementById('message');
async function request(url,body,csrf) {
  const response=await fetch(url,{method:body===undefined?'GET':'POST',credentials:'same-origin',headers:body===undefined?{}:{'Content-Type':'application/json',...(csrf?{'X-CSRF-Token':csrf}:{})},...(body===undefined?{}:{body:JSON.stringify(body)})});
  if(!response.ok){const error=new Error(response.status===429?t('Trop de tentatives. Réessayez dans une minute.'):t('Accès refusé. Vérifiez vos identifiants ou demandez un nouveau lien.'));error.status=response.status;throw error;}
  return response.json();
}
async function run(button,work){button.disabled=true;message.textContent='';try{await work();}catch(error){message.textContent=error.message;}finally{button.disabled=false;}}
const page=document.body.dataset.page;
if(page==='login')document.getElementById('login').addEventListener('submit',event=>{event.preventDefault();run(event.target.querySelector('button'),async()=>{await request('/auth/password/login',{name:document.getElementById('name').value,password:document.getElementById('password').value});document.getElementById('password').value='';location.assign('/');});});
if(page==='enroll'){
  const token=location.hash.slice(1);history.replaceState(null,'','/enroll');
  const form=document.getElementById('enroll'),minimum=Number(document.body.dataset.passwordMinLength),maximum=Number(document.body.dataset.passwordMaxLength);
  if(!token){message.textContent=t('Ce lien est incomplet. Demandez un nouveau lien à votre administrateur.');form.hidden=true;}
  form.addEventListener('submit',event=>{event.preventDefault();run(form.querySelector('button'),async()=>{const password=document.getElementById('password').value;if(password!==document.getElementById('confirm').value)throw new Error(t('Les deux mots de passe doivent être identiques.'));const length=[...password].length;if(length<minimum||length>maximum)throw new Error(t('Le mot de passe doit contenir entre {minimum} et {maximum} caractères.',{minimum,maximum}));await request('/auth/password/enroll',{token,password});form.reset();form.hidden=true;message.textContent=t('Mot de passe enregistré. Vous pouvez vous connecter.');});});
}
const states={running:t('En diffusion'),paused:t('En pause'),draining:t('Veille en attente'),sleeping:t('En veille')};
const connections={connecting:t('Connexion…'),online:t('Station connectée'),offline:t('Station inaccessible')};
const alerts={station_unreachable:t('StationD inaccessible'),onair_unavailable:t('Flux d’antenne indisponible'),stale_data:t('Dernières données connues — périmées'),liquidsoap_error:t('Erreur Liquidsoap'),icecast_error:t('Audience Icecast indisponible'),plugin_error:t('Un plugin est en échec'),no_liquidsoap:t('Liquidsoap non configuré'),pool_empty:t('Aucun média diffusable'),program_error:t('Erreur de calcul du programme')};
const notes={1:t('La station est en pause.'),2:t('La station est en veille.'),3:t('Veille à la fin du morceau.'),4:t('Veille dès qu’il n’y aura plus d’auditeur.'),5:t('Un DJ tient l’antenne.'),6:t('Liquidsoap n’est pas configuré.'),7:t('La suite est simulée.'),8:t('Aucun média diffusable.'),9:t('Programme de secours.'),10:t('Relais : durée inconnue.'),11:t('Une durée de média est inconnue.'),12:t('La simulation a échoué.'),13:t('Un filtre de plugin a échoué.'),14:t('La projection du programme a échoué.'),15:t('L’historique est indisponible.'),16:t('Un rendez-vous ne pourra pas couper l’antenne.'),17:t('Une source prévue sera vide.'),18:t('Un rendez-vous n’a pas coupé l’antenne.'),19:t('Une source était vide.')};
function mediaText(track,kind){if(track)return[track.title||t('Média sans titre'),track.artist].filter(Boolean).join(' — ');return({live:t('Direct DJ'),relay:t('Relais'),fallback:t('Secours'),halted:t('Fond d’arrêt'),unknown:t('Média non identifié')})[kind]||t('Aucun média signalé');}
function audienceText(value){return value==null?t('Audience inconnue'):t(value===1?'{value} auditeur':'{value} auditeurs',{value});}
function timeText(value,zone){if(!value)returnt('Inconnue');try{return new Date(value*1000).toLocaleTimeString(webminLanguage,{...(zone?{timeZone:zone}:{}),hour:'2-digit',minute:'2-digit',second:'2-digit'});}catch{return new Date(value*1000).toLocaleTimeString(webminLanguage);}}
function list(target,items){target.replaceChildren(...items.map(text=>{const item=document.createElement('li');item.textContent=text;return item;}));}
function descriptionList(target,entries){target.replaceChildren(...entries.flatMap(([name,value])=>{const term=document.createElement('dt'),data=document.createElement('dd');term.textContent=name;data.textContent=value;return[term,data];}));}
const cards=new Map();
let consoleEnabled=false;
function renderNetwork(data){
  const keep=new Set();
  for(const station of data.stations){
    keep.add(station.id);let card=cards.get(station.id);
    if(!card){const element=document.createElement('article');element.className='station';const title=document.createElement('h2'),link=document.createElement('a');link.href=`/station/${encodeURIComponent(station.id)}`;title.append(link);const status=document.createElement('p'),media=document.createElement('p'),audience=document.createElement('p'),observed=document.createElement('p'),warnings=document.createElement('ul');observed.className='muted';element.append(title,status,media,audience,observed,warnings);card={element,link,status,media,audience,observed,warnings};cards.set(station.id,card);document.getElementById('stations').append(element);}
    card.link.href=`/station/${encodeURIComponent(station.id)}${consoleEnabled&&station.role==='admin'?'/console':''}`;
    card.link.textContent=station.label;card.element.classList.toggle('stale',station.stale||station.connection==='offline');
    card.status.textContent=`${connections[station.connection]||t('Connexion inconnue')} · ${station.stale?t('Dernier état : '):''}${states[station.state]||t('État inconnu')}`;
    card.media.textContent=mediaText(station.media,station.on_air_kind);card.audience.textContent=audienceText(station.audience);
    card.observed.textContent=station.observed_at?t('Antenne observée à {time}',{time:timeText(station.observed_at)}):t('Aucune donnée d’antenne reçue');
    list(card.warnings,station.alerts.map(code=>alerts[code]||code));
  }
  for(const[id,card]of cards)if(!keep.has(id)){card.element.remove();cards.delete(id);}
  message.textContent=data.stations.length?'':t('Aucune station accessible.');
}
let detail;
function updateProgress(){const progress=document.getElementById('progress'),label=document.getElementById('progress-text');if(!progress)return;const track=detail?.media;if(detail?.stale||!track?.started_at||!track?.duration_ms||detail.state!=='running'){progress.hidden=true;label.textContent='';return;}const duration=track.duration_ms/1000,elapsed=Math.max(0,Math.min(duration,Date.now()/1000-track.started_at));progress.hidden=false;progress.max=duration;progress.value=elapsed;const format=s=>`${Math.floor(s/60)}:${String(Math.floor(s%60)).padStart(2,'0')}`;label.textContent=`${format(elapsed)} / ${format(duration)}`;}
function renderStation(station){
  detail=station;document.getElementById('station-name').textContent=station.label;document.title=`StationD — ${station.label}`;
  document.getElementById('station-state').textContent=`${connections[station.connection]} · ${station.stale?t('Dernier état : '):''}${states[station.state]||t('État inconnu')}`;
  document.getElementById('media').textContent=mediaText(station.media,station.on_air_kind);
  document.getElementById('audience').textContent=audienceText(station.audience);
  document.getElementById('observed').textContent=station.observed_at?t('Antenne observée à {time}',{time:timeText(station.observed_at,station.timezone)})+(station.stale?t(' — données périmées'):''):t('Aucune donnée d’antenne reçue');
  list(document.getElementById('alerts'),station.alerts.map(code=>alerts[code]||code));
  const upcoming=[];if(station.prefetched)upcoming.push(t('Préchargé (confirmé) : {media}',{media:mediaText(station.prefetched)}));for(const track of station.upcoming)upcoming.push(`${timeText(track.estimated_at,station.timezone)} · ${mediaText(track)}${t(' — simulé')}`);list(document.getElementById('upcoming'),upcoming.length?upcoming:[t('Aucune suite disponible.')]);
  list(document.getElementById('notes'),station.notes.map(code=>notes[code]||t('Information de programme (code {code}).',{code})));
  const serviceNames={online:t('Connecté'),offline:t('Inaccessible'),connecting:t('Connexion…'),ok:'OK',error:t('Erreur'),disabled:t('Non configuré'),unknown:t('Inconnu')};
  descriptionList(document.getElementById('services'),[['StationD',serviceNames[station.services.stationd]||t('Inconnu')],[t('Pont Liquidsoap'),station.services.liquidsoap==='ok'?t('Sans erreur signalée'):serviceNames[station.services.liquidsoap]||t('Inconnu')],['Icecast',serviceNames[station.services.icecast]||t('Inconnu')],['Plugins',station.services.plugins==='ok'?t('Aucun échec signalé'):serviceNames[station.services.plugins]||t('Inconnu')]]);
  document.getElementById('playlist').textContent=station.current_playlist?t('Programme courant : {name}',{name:station.current_playlist.name}):t('Aucun programme courant signalé.');
  list(document.getElementById('program'),station.next_playlists.map(slot=>`${timeText(slot.from,station.timezone)} · ${slot.name}${slot.issue?t(' — source indisponible'):''}`));updateProgress();
}
async function startDashboard(){
  let context;try{context=await request('/api/session');}catch(error){if(error.status===401||error.status===403)location.assign('/login');else message.textContent=t('Serveur indisponible. Rechargez la page pour réessayer.');return;}
  consoleEnabled=context.console_enabled===true;
  document.getElementById('account').textContent=context.name;
  document.getElementById('logout').addEventListener('click',event=>run(event.target,async()=>{await request('/auth/logout',{},context.csrf_token);location.assign('/login');}));
  const id=document.body.dataset.stationId,isNetwork=page==='network';
  if(!isNetwork){const grant=context.stations.find(station=>station.id===id);if(context.console_enabled&&grant?.role==='admin'){const link=document.getElementById('open-console');link.href=`/station/${encodeURIComponent(id)}/console`;link.hidden=false;}}
  const endpoint=isNetwork?'/api/stations':`/api/stations/${encodeURIComponent(id)}`;
  const render=isNetwork?renderNetwork:renderStation;
  try{render(await request(endpoint));}catch(error){if(error.status===401||error.status===403){location.assign('/login');return;}message.textContent=t('Lecture indisponible. Nouvelle tentative en cours…');}
  const source=new EventSource(isNetwork?'/api/network/events':`${endpoint}/events`);
  const connection=document.getElementById('connection');let checking=false;
  source.onopen=()=>{connection.textContent=t('Mise à jour en direct');connection.classList.remove('warning');};
  source.addEventListener('snapshot',event=>{try{render(JSON.parse(event.data));connection.textContent=t('Mise à jour en direct');connection.classList.remove('warning');}catch{connection.textContent=t('Données reçues illisibles.');}});
  source.addEventListener('session-ended',()=>{source.close();location.assign('/login');});
  source.onerror=()=>{connection.textContent=t('Connexion perdue — données affichées à vérifier. Reconnexion automatique…');connection.classList.add('warning');if(checking)return;checking=true;request('/api/session').catch(error=>{if(error.status===401||error.status===403){source.close();location.assign('/login');}}).finally(()=>{checking=false;});};
  window.addEventListener('pagehide',()=>source.close(),{once:true});
  if(!isNetwork)setInterval(updateProgress,1000);
}
if(page==='network'||page==='station')startDashboard();
