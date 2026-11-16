const frame = document.querySelector('iframe');
const status = document.querySelector('#status');
let current = null;

function show(message) {
  if (status.textContent !== message) status.textContent = message;
  status.hidden = !message;
}

async function refresh() {
  const controller = new AbortController();
  const deadline = window.setTimeout(() => controller.abort(), 4000);
  try {
    const response = await fetch('status.json', { signal: controller.signal });
    if (!response.ok) throw new Error('Preview unavailable');
    const next = await response.json();
    if (next.generation !== current) {
      current = next.generation;
      if (current === null) frame.removeAttribute('src');
      else frame.src = 'app/' + current + '/';
    }
    frame.hidden = current === null;
    if (next.failed) show('Build failed. Check the console in Freeflow.');
    else if (next.building) show(current === null ? 'Building the Flutter preview…' : 'Updating the preview…');
    else show('');
  } catch {
    frame.hidden = true;
    frame.removeAttribute('src');
    current = null;
    show('Preview disconnected. Check that Freeflow is sharing and both devices are on the same network.');
  } finally {
    window.clearTimeout(deadline);
    window.setTimeout(refresh, 1200);
  }
}

refresh();
