// Options page: the one-time pairing step for the HTTP backend.
//
// The extension cannot read the daemon's endpoint file, so the token has to cross by hand
// exactly once. Everything else about the backend is discovered automatically.

import {
  parsePairing,
  savePairing,
  loadPairing,
  probeDaemon,
  describeProbe,
  KokoroHttpNarrator,
} from './kokoro-http';

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const input = $<HTMLInputElement>('pairing');
const msg = $<HTMLSpanElement>('msg');
const current = $<HTMLParagraphElement>('current');

function show(text: string, cls: '' | 'ok' | 'bad' = '') {
  msg.textContent = text;
  msg.className = cls || 'muted';
}

async function refresh() {
  const p = await loadPairing();
  if (!p) {
    // Say whether there is anything to pair *with* before asking for a code - "start the daemon"
    // and "paste the code" are different jobs, and the empty field looks the same either way.
    current.textContent = 'Not paired - checking for a running daemon...';
    const probe = await probeDaemon();
    current.textContent = `Not paired - ${describeProbe(probe)}`;
    return;
  }
  current.textContent = `Paired with ${p.base}`;
  // Prove the daemon is actually reachable rather than just that a string was saved.
  try {
    const s = await new KokoroHttpNarrator(p).status();
    current.textContent = `Paired with ${p.base} - ${s.voices.length} voices, ${s.sampleRate} Hz`;
  } catch (e) {
    current.textContent = `Paired with ${p.base}, but it is not answering: ${String(e)}`;
  }
}

$<HTMLButtonElement>('save').addEventListener('click', async () => {
  const p = parsePairing(input.value);
  if (!p) {
    show('That does not look like a pairing string (kwr_<port>_<token>).', 'bad');
    return;
  }
  show('checking...');
  try {
    const s = await new KokoroHttpNarrator(p).status();
    await savePairing(p);
    input.value = '';
    show(`Paired - ${s.voices.length} voices.`, 'ok');
  } catch (e) {
    // Never save a pairing that does not work; a stored-but-broken endpoint is worse than none.
    show(`Could not reach the daemon: ${String(e)}`, 'bad');
  }
  void refresh();
});

$<HTMLButtonElement>('clear').addEventListener('click', async () => {
  await savePairing(null);
  show('Unpaired.', 'ok');
  void refresh();
});

void refresh();
