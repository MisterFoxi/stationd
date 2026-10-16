'use strict';
const filesInput = document.getElementById('files');
const drop = document.getElementById('drop');
const token = document.getElementById('token');
const send = document.getElementById('send');
const cancel = document.getElementById('cancel');
const queue = document.getElementById('queue');
const message = document.getElementById('message');
const count = document.getElementById('count');
let entries = [];
let running = false;
let stopping = false;
let active = null;
function updateButton() { send.disabled = running || !token.value || !entries.some(e => e.state === 'pending'); }
function addFiles(files) {
  if (running) return;
  for (const file of files) {
    const row = document.createElement('li');
    const title = document.createElement('strong');
    title.textContent = file.name;
    const size = document.createElement('span');
    size.className = 'size';
    size.textContent = `${(file.size / 1024 / 1024).toFixed(1)} Mo`;
    const progress = document.createElement('progress');
    progress.max = 100;
    progress.value = 0;
    progress.setAttribute('aria-label', `Transfert de ${file.name}`);
    const status = document.createElement('p');
    const valid = /\.(mp3|wav|aif|aiff|flac|ogg|oga|opus|m4a|aac|wma)$/i.test(file.name) && file.size > 0;
    status.textContent = valid ? 'En attente' : 'Format non pris en charge ou fichier vide';
    row.dataset.state = valid ? 'pending' : 'error';
    row.append(title, size, progress, status);
    queue.append(row);
    entries.push({file, row, progress, status, state: valid ? 'pending' : 'error'});
  }
  count.textContent = `${entries.length} fichier${entries.length > 1 ? 's' : ''}`;
  updateButton();
}
filesInput.addEventListener('change', () => { addFiles(filesInput.files); filesInput.value = ''; });
token.addEventListener('input', updateButton);
for (const event of ['dragenter', 'dragover']) drop.addEventListener(event, e => { e.preventDefault(); if (!running) drop.classList.add('drag'); });
for (const event of ['dragleave', 'drop']) drop.addEventListener(event, e => { e.preventDefault(); drop.classList.remove('drag'); });
drop.addEventListener('drop', e => addFiles(e.dataTransfer.files));
function transfer(entry, secret) {
  return new Promise(resolve => {
    const xhr = new XMLHttpRequest();
    active = xhr;
    entry.state = 'sending';
    entry.row.dataset.state = 'sending';
    entry.status.textContent = 'Transfert…';
    xhr.open('POST', `api/upload/${encodeURIComponent(entry.file.name)}`);
    xhr.setRequestHeader('Authorization', `Bearer ${secret}`);
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.upload.addEventListener('progress', e => {
      if (e.lengthComputable) entry.progress.value = Math.round(e.loaded * 100 / e.total);
      if (e.loaded === e.total) entry.status.textContent = 'Préparation du média · conversion en MP3 si nécessaire, tagging et scan…';
    });
    const finish = (state, text) => {
      entry.state = state;
      entry.row.dataset.state = state;
      entry.status.textContent = text;
      active = null;
      resolve();
    };
    xhr.addEventListener('load', () => {
      let result;
      try { result = JSON.parse(xhr.responseText); } catch { result = {}; }
      if (xhr.status === 201) {
        entry.progress.value = 100;
        finish(result.indexed ? 'done' : 'warning', result.indexed
          ? `Importé · Type uploaded · ${result.path}`
          : `Fichier enregistré · scan à vérifier dans la TUI · ${result.path}. Ne pas le renvoyer.`);
      } else {
        if (xhr.status === 401 || xhr.status === 429 || xhr.status === 503) stopping = true;
        const detail = result.staging_id ? ` Référence tampon : ${result.staging_id}` : '';
        finish('error', (result.error ? result.error + detail : null) || `Import refusé (HTTP ${xhr.status}). Vérifiez le journal StationD.`);
      }
    });
    xhr.addEventListener('error', () => { stopping = true; finish('error', 'Connexion perdue. Vérifiez la TUI ou la zone tampon avant de renvoyer ce fichier.'); });
    xhr.addEventListener('abort', () => finish('warning', 'Transfert arrêté. Après réception complète, vérifiez la TUI ou la zone tampon avant de renvoyer le fichier.'));
    xhr.send(entry.file);
  });
}
send.addEventListener('click', async () => {
  if (running || send.disabled) return;
  if (!/^[\x21-\x7e]{32,512}$/.test(token.value)) {
    message.textContent = 'La clé doit contenir entre 32 et 512 caractères ASCII sans espace.';
    token.focus();
    return;
  }
  running = true;
  stopping = false;
  const secret = token.value;
  token.disabled = true;
  filesInput.disabled = true;
  cancel.hidden = false;
  updateButton();
  message.textContent = 'Import en cours…';
  try {
    for (const entry of entries) {
      if (stopping) break;
      if (entry.state === 'pending') await transfer(entry, secret);
    }
  } finally {
    running = false;
    token.disabled = false;
    filesInput.disabled = false;
    cancel.hidden = true;
    const done = entries.filter(e => e.state === 'done').length;
    const warnings = entries.filter(e => e.state === 'warning' || e.state === 'error').length;
    message.textContent = `${stopping ? 'Transferts arrêtés.' : 'Transferts terminés.'} ${done} importé(s), ${warnings} à vérifier.`;
    updateButton();
  }
});
cancel.addEventListener('click', () => { stopping = true; if (active) active.abort(); });
window.addEventListener('beforeunload', e => { if (running) { e.preventDefault(); e.returnValue = ''; } });