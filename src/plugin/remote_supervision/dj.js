/* Private DJ preparation and monitor. No passwords or tickets in URLs/storage. */
'use strict';
const el=id=>document.getElementById(id);
const stationId=document.body.dataset.stationId;
const endpoint=`/api/stations/${encodeURIComponent(stationId)}/dj`;
let context,authenticated=false,busy=false,previewing=false,pollTimer,lastState,settingsKey,disposed=false;
const labels={waiting:'En attente de votre flux',checking:'Vérification du flux en cours',silent:'Aucun son détecté',unstable:'Flux indisponible ou instable',ready:'Votre flux est bon, vous pouvez prendre l’antenne',taking:'Prise d’antenne en cours',on_air:'Vous êtes à l’antenne',busy:'Un autre DJ est connecté',closed:'Votre créneau de connexion est terminé'};
const help={waiting:'Lancez la diffusion dans votre logiciel. La programmation continue pendant votre préparation.',checking:'StationD vérifie plusieurs secondes de son continu avant de vous donner le feu vert.',silent:'Vérifiez le gain, la sortie audio et la musique dans votre logiciel.',unstable:'Vérifiez la connexion et attendez que StationD reçoive du son décodable.',ready:'Vous pouvez écouter votre flux et ajuster votre son avant de prendre l’antenne.',taking:'Attendez la confirmation de StationD.',on_air:'Votre flux est diffusé aux auditeurs. Déconnectez-le pour rendre l’antenne.',busy:'La prise d’antenne est indisponible tant que ce DJ est connecté.',closed:'Contactez l’administrateur pour obtenir une nouvelle ouverture.'};
async function request(url,body){
  const controller=new AbortController(),timer=setTimeout(()=>controller.abort(),5000);
  try{
    const response=await fetch(url,{credentials:'same-origin',cache:'no-store',signal:controller.signal,
      ...(body!==undefined?{method:'POST',headers:{'Content-Type':'application/json','X-CSRF-Token':context.csrf_token},body:JSON.stringify(body)}:{})});
    if(!response.ok){const error=new Error('request');error.status=response.status;throw error;}
    return await response.json();
  }finally{clearTimeout(timer);}
}
function stopPreview(){previewing=false;const audio=el('preview-audio');audio.pause();audio.removeAttribute('src');audio.load();audio.hidden=true;el('preview-stop').disabled=true;el('preview-status').textContent='';el('preview').disabled=!lastState?.can_preview||busy;}
function render(state){
  lastState=state;authenticated=true;el('dj-login').hidden=true;el('workspace').hidden=false;
  el('signal-state').textContent=t(labels[state.state]||labels.unstable);
  el('signal-help').textContent=t(help[state.state]||help.unstable)+(state.other_dj?` (${state.other_dj})`:'');
  el('signal-panel').dataset.signal=state.state;
  el('take').disabled=busy||!state.can_take;el('stop').disabled=busy||!['checking','silent','unstable','ready','taking','on_air','closed'].includes(state.state);
  el('preview').disabled=busy||!state.can_preview||previewing;
  el('level').value=state.level_db;el('level-text').textContent=`${state.level_db.toFixed(1)} dBFS`;
  el('clipping').hidden=!state.clipping;el('legacy').hidden=state.manual_takeover;
  el('delay').textContent=t('Le retour est décalé : {seconds} s de tampon serveur, plus le délai du lecteur.',{seconds:state.buffer_seconds});
  if(previewing&&!state.can_preview)stopPreview();
  const settings=[['Serveur',state.host||t('Adresse à confirmer')],['Port',String(state.port)],['Point de montage',state.mount],['Utilisateur',state.dj]];
  const nextSettings=JSON.stringify(settings);
  if(settingsKey!==nextSettings){settingsKey=nextSettings;el('encoder-settings').replaceChildren(...settings.flatMap(([label,value])=>{
    const term=document.createElement('dt'),data=document.createElement('dd'),text=document.createElement('span');
    term.textContent=t(label);text.textContent=value;data.append(text);
    if(value&&!(label==='Serveur'&&!state.host)){const copy=document.createElement('button');copy.type='button';copy.className='dj-copy';copy.textContent=t('Copier');copy.setAttribute('aria-label',t('Copier {field}',{field:t(label)}));copy.addEventListener('click',async()=>{try{await navigator.clipboard.writeText(value);el('message').textContent=t('Paramètre copié.');}catch{el('message').textContent=t('Sélectionnez le paramètre pour le copier.');}});data.append(copy);}
    return[term,data];
  }));}el('host-help').hidden=!!state.host;
}
function unavailable(error){
  el('take').disabled=true;el('stop').disabled=true;el('preview').disabled=true;stopPreview();
  // Never leave a stale green readiness indicator after a failed request.
  lastState=undefined;el('preview').disabled=true;el('signal-panel').dataset.signal='unstable';
  el('signal-state').textContent=t(labels.unstable);el('signal-help').textContent=t(help.unstable);el('clipping').hidden=true;el('level').value=-90;el('level-text').textContent='— dBFS';
  if(error.status===401){authenticated=false;el('dj-login').hidden=false;el('workspace').hidden=true;el('message').textContent=t('Identifiez-vous avec votre accès DJ.');}
  else el('message').textContent=t(error.status===409?'Le flux ou le créneau n’est plus prêt. Vérifiez l’état avant de réessayer.':'Station indisponible. Nouvelle vérification en cours…');
}
async function poll(){
  if(disposed)return;
  if(authenticated&&!busy){try{render(await request(endpoint));}catch(error){unavailable(error);}}
  if(!disposed)pollTimer=setTimeout(poll,2000);
}
async function action(name){
  if(busy)return;busy=true;el('take').disabled=true;el('stop').disabled=true;el('message').textContent='';
  try{render(await request(`${endpoint}/${name}`,{}));if(name==='stop')stopPreview();}
  catch(error){unavailable(error);}finally{busy=false;if(lastState)render(lastState);}
}
async function start(){
  try{context=await request('/api/session');}catch{location.assign('/login');return;}
  const station=context.stations.find(s=>s.id===stationId);if(!station){location.assign('/');return;}
  el('station-label').textContent=station.label;el('account').textContent=context.name;
  el('logout').addEventListener('click',async()=>{stopPreview();try{await request('/auth/logout',{});location.assign('/login');}catch{el('message').textContent=t('Déconnexion indisponible. Réessayez.');}});
  el('login-form').addEventListener('submit',async event=>{
    event.preventDefault();if(busy)return;busy=true;el('login-button').disabled=true;el('message').textContent='';
    const password=el('dj-password').value;el('dj-password').value='';
    try{render(await request(`${endpoint}/login`,{dj:el('dj-name').value,password}));}
    catch(error){el('message').textContent=t(error.status===401?'Identifiants DJ refusés.':error.status===429?'Trop de tentatives. Patientez avant de réessayer.':'Station indisponible. Réessayez.');}
    finally{busy=false;el('login-button').disabled=false;if(lastState)render(lastState);}
  });
  el('take').addEventListener('click',()=>action('take'));el('stop').addEventListener('click',()=>action('stop'));
  el('preview-stop').addEventListener('click',stopPreview);
  el('preview').addEventListener('click',async()=>{
    if(!lastState?.can_preview)return;previewing=true;el('preview').disabled=true;el('preview-stop').disabled=false;
    const audio=el('preview-audio');audio.hidden=false;audio.src=`${endpoint}/preview`;el('preview-status').textContent=t('Connexion à la préécoute…');
    try{await audio.play();if(previewing)el('preview-status').textContent=t('Vous écoutez votre flux reçu par StationD.');}
    catch{if(previewing){stopPreview();el('preview-status').textContent=t('Préécoute indisponible. Vérifiez le flux et réessayez.');}}
  });
  el('preview-audio').addEventListener('error',()=>{if(previewing){stopPreview();el('preview-status').textContent=t('Préécoute interrompue. Relancez l’écoute.');}});
  el('preview-audio').addEventListener('ended',()=>{if(previewing){stopPreview();el('preview-status').textContent=t('Préécoute interrompue. Relancez l’écoute.');}});
  try{render(await request(endpoint));}catch(error){if(error.status!==401)unavailable(error);}
  poll();
  window.addEventListener('pagehide',()=>{disposed=true;clearTimeout(pollTimer);authenticated=false;stopPreview();},{once:true});
}
start();
