import init, { WebHandle } from './pkg/stationdeck.js';

const loading = document.getElementById('loading');
const message = document.getElementById('loading-message');
const canvas = document.getElementById('stationdeck');
try {
  await init();
  const handle = new WebHandle();
  await handle.start(canvas);
  loading.hidden = true;
  canvas.dataset.ready = 'true';
  canvas.focus();
  window.addEventListener('pagehide', () => handle.destroy(), { once: true });
} catch (error) {
  canvas.hidden = true;
  message.textContent = 'Unable to start StationDeck. Check  WebAssembly znd WebGL, then reload.';
  loading.dataset.error = 'true';
  console.error('StationDeck startup failed:', error);
}
