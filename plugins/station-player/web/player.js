'use strict';
const slug = decodeURIComponent(location.pathname.split('/').filter(Boolean).pop());
const api = '/api/public/player/' + encodeURIComponent(slug);
const $ = id => document.getElementById(id);
const compact = new URLSearchParams(location.search).get('compact') === '1';
document.body.classList.toggle('compact', compact);
$('full').href = '/player/' + encodeURIComponent(slug);
const audio = $('audio');
let timezone, latest;
const time = epoch => {
  if (epoch == null) return '';
  try { return new Intl.DateTimeFormat('fr-FR', {hour:'2-digit', minute:'2-digit', timeZone:timezone || undefined}).format(new Date(epoch * 1000)); }
  catch { return new Date(epoch * 1000).toLocaleTimeString('fr-FR'); }
};
function list(id, items, agenda = false, history = false) {
  const rows = items.map(item => {
    const li = document.createElement('li'), title = document.createElement('strong'), detail = document.createElement('span');
    title.textContent = agenda ? item.label : item.title || 'Titre non renseigné';
    detail.textContent = agenda ? time(item.at) || 'Horaire indicatif' : [item.artist, time(history ? item.started_at : item.estimated_at)].filter(Boolean).join(' · ');
    li.append(title, detail); return li;
  });
  if (!rows.length) { const li = document.createElement('li'); li.textContent = 'Aucune information disponible'; rows.push(li); }
  $(id).replaceChildren(...rows);
}
function freshness() {
  if (!latest) return;
  const stale = latest.stale || !latest.air.observed_at || Date.now() / 1000 - latest.air.observed_at > 60;
  $('freshness').textContent = stale ? 'Informations du direct indisponibles ou anciennes.' : 'Mis à jour à ' + time(latest.air.observed_at);
  if (stale) $('status').textContent = 'Informations hors ligne';
}
function render(data) {
  latest = data;
  const {station, air} = data; timezone = air.timezone;
  document.title = station.label + ' · Direct';
  $('station').textContent = station.label; $('description').textContent = station.description;
  for (const key of ['accent', 'background', 'foreground']) document.documentElement.style.setProperty('--' + key, station.theme[key]);
  // Metadata updates must never reload a playing Icecast connection.
  if (audio.getAttribute('src') !== station.stream_url) audio.src = station.stream_url;
  const kinds = {live:'Émission en direct', relay:'Relais en direct', fallback:'Programme de secours'};
  $('title').textContent = air.now_playing?.title || kinds[air.kind] || 'En attente du direct';
  $('artist').textContent = air.now_playing?.artist || '';
  const states = {paused:'Station en pause', sleeping:'Station en veille', draining:'Fin de diffusion en cours'};
  $('status').textContent = states[air.state] || (data.status === 'online' ? 'En direct' : 'Connexion au direct…');
  list('next', air.coming_next); list('indicative', air.indicative); list('agenda', air.agenda, true);
  list('indicative-agenda', air.indicative_agenda, true); list('history', air.history, false, true);
  freshness();
}
for (const [event, message] of Object.entries({playing:'Écoute en cours',waiting:'Chargement du flux…',stalled:'Le flux tarde à répondre.',error:'Flux indisponible. Relancez la lecture pour réessayer.',pause:'Lecture en pause'})) {
  audio.addEventListener(event, () => { $('audio-status').textContent = message; });
}
// Native controls provide keyboard access, volume and user-initiated playback.
fetch(api, {cache:'no-store'}).then(r => { if (!r.ok) throw Error(); return r.json(); }).then(data => { if (!latest) render(data); }).catch(() => {
  if (!latest) $('status').textContent = 'Station indisponible';
});
const events = new EventSource(api + '/events');
events.addEventListener('snapshot', event => { try { render(JSON.parse(event.data)); } catch { $('freshness').textContent = 'Mise à jour indisponible'; } });
events.onerror = () => { $('freshness').textContent = 'Reconnexion aux informations du direct…'; };
setInterval(freshness, 15000);
window.addEventListener('pagehide', () => events.close());
