'use strict';
const slug = decodeURIComponent(location.pathname.split('/').filter(Boolean).pop());
const api = '/api/public/player/' + encodeURIComponent(slug);
const $ = id => document.getElementById(id);
const compact = new URLSearchParams(location.search).get('compact') === '1';
document.body.classList.toggle('compact', compact);
$('full').href = '/player/' + encodeURIComponent(slug);
const audio = $('audio');
const artwork = $('artwork');
let timezone, latest, failedArtwork, historyExpanded = false;
const time = epoch => {
  if (epoch == null) return '';
  try { return new Intl.DateTimeFormat('fr-FR', {hour:'2-digit', minute:'2-digit', timeZone:timezone || undefined}).format(new Date(epoch * 1000)); }
  catch { return new Date(epoch * 1000).toLocaleTimeString('fr-FR'); }
};
function list(id, items, {agenda = false, history = false, empty = 'Aucun titre annoncé pour le moment.'} = {}) {
  const rows = items.map(item => {
    const li = document.createElement('li');
    const title = document.createElement('strong');
    const detail = document.createElement('span');
    const clock = document.createElement('time');
    title.textContent = agenda ? item.label : item.title || 'Titre non renseigné';
    detail.className = 'track-meta';
    detail.textContent = agenda ? 'Rendez-vous indicatif' : item.artist || 'Artiste non renseigné';
    const epoch = agenda ? item.at : history ? item.started_at : item.estimated_at;
    clock.textContent = time(epoch) || (agenda ? '—' : '');
    if (epoch != null) { const date = new Date(epoch * 1000); if (!Number.isNaN(date.getTime())) clock.dateTime = date.toISOString(); }
    li.append(title, detail, clock);
    return li;
  });
  if (!rows.length) {
    const li = document.createElement('li');
    li.className = 'empty';
    li.textContent = empty;
    rows.push(li);
  }
  $(id).replaceChildren(...rows);
}
function freshness() {
  if (!latest) return;
  const stale = latest.stale || !latest.air.observed_at || Date.now() / 1000 - latest.air.observed_at > 60;
  $('freshness').textContent = stale ? 'Informations du direct indisponibles ou anciennes.' : 'Mis à jour à ' + time(latest.air.observed_at);
  document.body.dataset.connection = stale ? 'offline' : 'online';
  if (stale) $('status').textContent = 'Informations hors ligne';
}
function renderHistory() {
  if (!latest) return;
  const items = latest.air.history;
  list('history', historyExpanded ? items : items.slice(0, 8), {history:true, empty:'Les derniers passages s’afficheront ici.'});
  $('history-more').hidden = items.length <= 8;
  $('history-more').textContent = historyExpanded ? 'Réduire ↑' : 'Voir les ' + items.length + ' passages ↗';
  $('history-more').setAttribute('aria-expanded', String(historyExpanded));
}
function render(data) {
  latest = data;
  const {station, air} = data;
  timezone = air.timezone;
  document.title = station.label + ' · Direct';
  $('station').textContent = station.label;
  $('description').textContent = station.description;
  for (const key of ['accent', 'background', 'foreground']) document.documentElement.style.setProperty('--' + key, station.theme[key]);
  // Changing metadata never reloads an Icecast connection that is playing.
  if (audio.getAttribute('src') !== station.stream_url) audio.src = station.stream_url;
  const cover = station.artwork_url && station.artwork_url !== failedArtwork ? station.artwork_url : null;
  artwork.hidden = !cover;
  document.body.classList.toggle('has-artwork', !!cover);
  if (cover && artwork.getAttribute('src') !== cover) artwork.src = cover;
  if (!cover) artwork.removeAttribute('src');
  document.body.dataset.stationState = air.state;
  const states = {paused:'Station en pause', sleeping:'Station en veille', draining:'Fin de diffusion en cours'};
  const kinds = {live:'Émission en direct', relay:'Relais en direct', fallback:'Programme de secours'};
  $('status').textContent = states[air.state] || (data.status === 'online' ? 'En direct' : 'Connexion au direct…');
  $('onair-label').textContent = air.state === 'sleeping' ? 'VOTRE RADIO · EN VEILLE' : air.state === 'paused' ? 'VOTRE RADIO · EN PAUSE' : 'À L’ANTENNE';
  $('title').textContent = air.now_playing?.title || kinds[air.kind] || (air.state === 'sleeping' ? 'Une pause sur votre fréquence.' : air.state === 'paused' ? 'Le direct fait une pause.' : 'La musique commence ici.');
  $('artist').textContent = air.now_playing?.artist || (states[air.state] ? 'Le direct est actuellement ' + (air.state === 'sleeping' ? 'en veille.' : air.state === 'paused' ? 'en pause.' : 'en fin de diffusion.') : 'Installez-vous. On s’occupe de la suite.');
  const emptyQueue = air.state === 'sleeping' ? 'La station est en veille. Les prochains titres apparaîtront à la reprise du direct.' : air.state === 'paused' ? 'La diffusion est en pause pour le moment.' : 'Le prochain titre s’affichera dès qu’il sera préparé.';
  $('next-count').textContent = String(air.coming_next.length);
  list('next', air.coming_next, {empty:emptyQueue});
  list('indicative', air.indicative.slice(0, 6), {empty:states[air.state] ? 'Les prévisions reprendront avec le direct.' : 'La suite du programme se prépare.'});
  list('agenda', air.agenda.slice(0, 5), {agenda:true, empty:'Aucun rendez-vous annoncé pour le moment.'});
  $('agenda-more').hidden = air.agenda.length <= 5;
  list('agenda-rest', air.agenda.slice(5), {agenda:true});
  list('indicative-agenda', air.indicative_agenda, {agenda:true, empty:'Aucun autre rendez-vous annoncé.'});
  $('agenda-zone').textContent = 'Horaires de la station' + (timezone ? ' · ' + timezone : '') + '.';
  renderHistory();
  freshness();
}
function audioControls(playing = !audio.paused, active = !audio.paused && !audio.error) {
  document.body.classList.toggle('is-playing', playing);
  $('play').setAttribute('aria-pressed', String(active));
  $('play').setAttribute('aria-label', active ? 'Mettre en pause' : 'Écouter le direct');
  $('play-label').textContent = playing ? 'Vous êtes à l’écoute' : active ? 'Connexion au direct…' : 'Écouter le direct';
}
$('play').addEventListener('click', async () => {
  if (!audio.paused && !audio.error) { audio.pause(); audio.load(); return; }
  if (audio.error) audio.load();
  $('audio-status').textContent = 'Connexion au flux…';
  try { const request = audio.play(); audioControls(false, true); await request; audioControls(!audio.paused && !audio.error); }
  catch (error) {
    if (error.name === 'AbortError' && audio.paused) return;
    audioControls(false, false);
    $('audio-status').textContent = 'Flux indisponible. Relancez la lecture pour réessayer.';
  }
});
function volumeControls() {
  $('volume').value = String(audio.volume);
  $('mute').dataset.muted = String(audio.muted);
  $('mute').setAttribute('aria-pressed', String(audio.muted));
  $('mute').setAttribute('aria-label', audio.muted ? 'Rétablir le son' : 'Couper le son');
}
audio.volume = Number($('volume').value);
$('volume').addEventListener('input', () => { audio.volume = Number($('volume').value); audio.muted = false; volumeControls(); });
$('mute').addEventListener('click', () => { audio.muted = !audio.muted; volumeControls(); });
audio.addEventListener('volumechange', volumeControls);
audio.addEventListener('play', () => audioControls(false, true));
for (const [event, message] of Object.entries({playing:'Le direct est à vous.',waiting:'Le flux arrive…',stalled:'Le flux tarde à répondre.',error:'Flux indisponible. Relancez la lecture pour réessayer.',pause:'À tout de suite.'})) {
  audio.addEventListener(event, () => {
    $('audio-status').textContent = message;
    audioControls(event === 'playing', event === 'error' || event === 'pause' ? false : !audio.paused);
  });
}
artwork.addEventListener('error', () => {
  failedArtwork = artwork.getAttribute('src');
  artwork.hidden = true;
  document.body.classList.toggle('has-artwork', false);
});
$('history-more').addEventListener('click', () => { historyExpanded = !historyExpanded; renderHistory(); });
// Progressive enhancement: native controls remain usable if this script cannot run.
audio.controls = false;
$('custom-controls').hidden = false;
document.body.classList.toggle('enhanced', true);
volumeControls();
fetch(api, {cache:'no-store'}).then(r => { if (!r.ok) throw Error(); return r.json(); }).then(data => { if (!latest) render(data); }).catch(() => {
  if (!latest) $('status').textContent = 'Station indisponible';
});
const events = new EventSource(api + '/events');
events.addEventListener('snapshot', event => { try { render(JSON.parse(event.data)); } catch { $('freshness').textContent = 'Mise à jour indisponible'; } });
events.onerror = () => { $('freshness').textContent = 'Reconnexion aux informations du direct…'; };
setInterval(freshness, 15000);
window.addEventListener('pagehide', () => events.close());
