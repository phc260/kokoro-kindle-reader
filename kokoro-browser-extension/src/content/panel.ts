// Foldable in-page control panel.
//
// Lives inside its own shadow root, for symmetry with the rest of the project: Amazon's CSS
// cannot reach in and restyle it, and ours cannot leak out and disturb their layout. This host
// element and highlight.ts's are the only two things this extension adds to their DOM.
//
// Note it adds one shadow host to the page, so `selftest()` will report 39 rather than 37 while
// the panel and the highlight are both mounted. Neither contains blob: images, so candidate
// scoring is unaffected.

import type { SpeakOptions, VoiceInfo } from '../speak';
import { buildVoiceTree, locateVoice, type AccentBucket } from '../voices';
import type { Position } from './capture';

export interface PanelActions {
  /** Which engine is speaking, for the status line. */
  engine(): string;
  /** Why the native backend was not used, if it wasn't. */
  engineError(): string | null;
  /**
   * Read from the current page onward. There is deliberately no single-page action: stopping at
   * a page boundary is what Stop is for, and a reader who wants one page presses Stop when it
   * ends. `kwr.speakPage()` still exists for the console.
   */
  readBook(opts?: SpeakOptions): Promise<void>;
  stop(): void;
  pause(): void;
  resume(): void;
  /**
   * Say one sample sentence in the selected voice.
   *
   * A single utterance, not a page: it is how a voice is chosen without opening a book, which is
   * the whole reason the panel is on the library at all. Resolves when the sample finishes or is
   * stopped, and rejects with something worth showing if it never speaks - a preview that fails
   * silently is indistinguishable from a voice that is merely quiet.
   */
  preview(text: string, opts?: SpeakOptions): Promise<void>;
  /**
   * Is anything being spoken that the user has not stopped?
   *
   * Not the same question as the panel's own `reading` flag, which only knows about presses it
   * saw. `kwr.readBook()` and `kwr.speakPage()` are reachable from the console and the
   * page-world debug bridge without the panel hearing about it, and a Preview started on top of
   * one would not play alongside it - the worker bumps its generation and calls `closeAll()`,
   * ending the book. "That the user has not stopped" rather than "that has not finished",
   * because an utterance is not guaranteed to finish - see `narrate.narrating()`.
   */
  narrating(): boolean;
  /**
   * Is a book open and rendered RIGHT NOW?
   *
   * Asked at press time, because the panel's own `bookOpen` is refreshed by a 500 ms poll and is
   * therefore up to half a second stale - and Play is a different action on each side of it. Open
   * a book and press Play before the next tick, and the cached flag would have played a voice
   * sample instead of reading the page that is on screen.
   */
  hasBook(): boolean;
  /** Apply a new speed to the page already being read. See `narrate.retune`. */
  retune(rate: number): void;
  voices(): Promise<VoiceInfo[]>;
  position(): Position;
}

export interface PanelHandle {
  destroy(): void;
  status(text: string, tone?: 'idle' | 'busy' | 'error'): void;
  /**
   * Whether a book is open and rendered under the panel.
   *
   * The panel outlives the book: it is mounted on the library too, so the voice, the speed and
   * whether Kokoro is reachable can all be settled before a book is opened. What does not
   * outlive the book is the transport - Play would capture a page that is not there - so it is
   * disabled until there is one.
   */
  setBookOpen(open: boolean): void;
}

type Prefs = { folded: boolean; voice: string; rate: number };

const PREFS_KEY = 'kwr.panel.prefs';
const HOST_ID = 'kokoro-kindle-cloud-reader-panel';

function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (raw) return { folded: false, voice: '', rate: 1, ...(JSON.parse(raw) as Partial<Prefs>) };
  } catch {
    // Private mode or a storage-blocked origin: defaults are fine, never fail the panel.
  }
  return { folded: false, voice: '', rate: 1 };
}

function savePrefs(p: Prefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p));
  } catch {
    /* ignore */
  }
}

/**
 * The speed readout: a percentage of normal, not a multiplier.
 *
 * The slider spans 0.5-2.0 in 0.1 steps, so this reads 50% to 200% in 10-point moves. Rounded
 * because a 0.1 step lands on values like 1.2000000000000002, which would otherwise show as
 * 120.00000000000003%.
 *
 * The wire value is untouched - `rate` stays a multiplier all the way to the model, which is
 * what it is. Only the label changes.
 */
const fmtRate = (rate: number): string => `${Math.round(rate * 100)}%`;

/**
 * The preview sentence.
 *
 * It names the voice on purpose. Choosing between narrators is done by playing two of them a
 * few seconds apart, and by the second one you no longer remember which row you were on - so the
 * sample says. Kept to one sentence because every preview is a real synthesis on the one worker
 * Kindle also queues behind.
 */
const sampleFor = (name: string): string => `Hello, I'm ${name}. This is how your book will sound.`;

const CSS = `
:host { all: initial; }
.wrap {
  position: fixed; right: 16px; bottom: 16px; z-index: 2147483647;
  font: 13px/1.4 system-ui, -apple-system, "Segoe UI", sans-serif;
  color: #e8e8ea; background: #1c1c20; border: 1px solid #34343a;
  border-radius: 12px; box-shadow: 0 8px 28px rgba(0,0,0,.45);
  width: 260px; overflow: hidden;
  transition: width .15s ease;
}
/* Folded, the panel is just the status dot + the title, so its width is set by the title. 180px
   fits "Kokoro Kindle Reader" (118px at 600 12px, measured) plus the dot, the chevron, the two
   gaps and the padding. Shrink this and the name clips - the point of the folded state is that
   you can still tell what the thing is. */
.wrap.folded { width: 180px; }
header {
  display: flex; align-items: center; gap: 8px;
  padding: 8px 10px; cursor: pointer; user-select: none;
  background: #24242a; border-bottom: 1px solid #34343a;
}
.wrap.folded header { border-bottom: none; }
.dot { width: 8px; height: 8px; border-radius: 50%; background: #4b8; flex: none; }
.dot.busy { background: #fb4; animation: pulse 1s infinite; }
.dot.error { background: #f55; }
@keyframes pulse { 50% { opacity: .35; } }
/* NB: this whole block is a JS template literal - no backticks in these comments.
   system-ui is a different typeface per OS - Segoe UI here, San Francisco on macOS, whatever the
   desktop set on Linux - so the title's width is not the same everywhere and the 180px above is
   calibrated on Windows. Ellipsis rather than nowrap alone, so a wider face degrades to
   "Kokoro Kindle Rea..." instead of being silently cut mid-glyph by the wrapper's overflow. */
h1 {
  font-size: 12px; font-weight: 600; margin: 0; flex: 1; letter-spacing: .02em;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
.chev { transition: transform .15s ease; opacity: .6; flex: none; }
.wrap.folded .chev { transform: rotate(180deg); }
.body { padding: 10px; display: grid; gap: 9px; }
.wrap.folded .body { display: none; }
.row { display: flex; gap: 6px; }
button {
  font: inherit; color: #e8e8ea; background: #303038; border: 1px solid #3d3d45;
  border-radius: 7px; padding: 7px 9px; cursor: pointer; flex: 1;
}
button:hover:not(:disabled) { background: #3a3a44; }
button:disabled { opacity: .4; cursor: default; }
/* Transport: outlined icon buttons, one hue each. The label lives in title/aria-label only, so
   colour is doing real work here, not decoration - it is the fastest way to tell Play from
   Resume, whose glyphs are near-identical by nature. Never the ONLY signal though: the enabled
   state and the tooltip both say the same thing, for anyone who cannot separate the hues.
   Equal flex keeps the four the same width however the panel is sized. */
/* A fixed 12 px apart, packed from the left edge like every other row starts. Distributing them
   across the full width instead put 32 px between neighbours, which read as four separate
   controls rather than one transport group. */
.transport { gap: 12px; }
.transport button {
  display: flex; align-items: center; justify-content: center;
  /* Fixed square. flex:none overrides the shared flex:1, which would otherwise stretch them to
     fill the row; padding goes to zero so the box is exactly the stated size. */
  flex: none; width: 36px; height: 36px; padding: 0;
  color: var(--c); background: transparent; border: 1.5px solid var(--c);
}
.transport button:hover:not(:disabled) { background: var(--tint); }
.transport button:disabled { opacity: .1; }
.transport svg { display: block; fill: currentColor; }
[data-act="play"]   { --c: #4da3ff; --tint: rgba(77,163,255,.16); }
[data-act="pause"]  { --c: #ffb340; --tint: rgba(255,179,64,.16); }
[data-act="resume"] { --c: #4ade80; --tint: rgba(74,222,128,.16); }
[data-act="stop"]   { --c: #ff6b6b; --tint: rgba(255,107,107,.16); }
label { display: grid; gap: 3px; font-size: 11px; opacity: .75; }
.lbl { display: flex; justify-content: space-between; align-items: baseline; }
.lbl b { font-weight: 600; opacity: .9; white-space: nowrap; }
select, input[type=range] { font: inherit; width: 100%; }
select {
  color: #e8e8ea; background: #26262c; border: 1px solid #3d3d45;
  border-radius: 6px; padding: 4px;
}
/* min-width:0 or a long option name refuses to shrink and pushes the row past the panel. */
.row select { flex: 1; min-width: 0; }
/* Chrome renders the popup with the select's own colours; without this the rows come out
   black-on-black. */
option { color: #e8e8ea; background: #26262c; }
.status {
  font-size: 11px; opacity: .75; min-height: 15px;
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.status.error { color: #f88; opacity: 1; white-space: normal; }
@media (prefers-color-scheme: light) {
  .wrap { color: #1c1c20; background: #fbfbfd; border-color: #d8d8de; }
  header { background: #f0f0f4; border-bottom-color: #d8d8de; }
  button { color: #1c1c20; background: #e9e9ef; border-color: #d2d2da; }
  button:hover:not(:disabled) { background: #dedee6; }
  /* The transport keeps its dark-mode hues deliberately - one colour per action, whatever the
     scheme, so the buttons are recognisable at a glance in either. */
  select { color: #1c1c20; background: #fff; border-color: #d2d2da; }
  option { color: #1c1c20; background: #fff; }
}
`;

const HTML = `
<div class="wrap" part="wrap">
  <header title="Click to fold">
    <span class="dot"></span>
    <h1>Kokoro Kindle Reader</h1>
    <svg class="chev" width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
      <path d="M1 3.5 5 7 9 3.5" fill="none" stroke="currentColor" stroke-width="1.6"/>
    </svg>
  </header>
  <div class="body">
    <div class="row transport">
      <button data-act="play" title="Read from this page on" aria-label="Play">
        <svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M4.5 2.8 13 8 4.5 13.2Z"/>
        </svg>
      </button>
      <button data-act="pause" title="Pause" aria-label="Pause" disabled>
        <svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="4" y="3" width="3" height="10" rx="1"/><rect x="9" y="3" width="3" height="10" rx="1"/>
        </svg>
      </button>
      <button data-act="resume" title="Resume" aria-label="Resume" disabled>
        <svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="3" y="3" width="2.6" height="10" rx="1"/><path d="M7.8 3.4 14 8 7.8 12.6Z"/>
        </svg>
      </button>
      <button data-act="stop" title="Stop" aria-label="Stop" disabled>
        <svg width="15" height="15" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="3.5" y="3.5" width="9" height="9" rx="1.5"/>
        </svg>
      </button>
    </div>
    <label>Voice
      <div class="row">
        <select data-el="accent"></select>
        <select data-el="gender"></select>
      </div>
      <select data-el="voice"><option value="">Loading…</option></select>
    </label>
    <label>
      <span class="lbl">Speed <b data-el="rateval">100%</b></span>
      <input type="range" data-el="rate" min="0.5" max="2" step="0.1" value="1">
    </label>
    <div class="status" data-el="status"></div>
  </div>
</div>`;

export function mountPanel(actions: PanelActions, initial: { bookOpen: boolean }): PanelHandle {
  document.getElementById(HOST_ID)?.remove(); // never mount twice

  const host = document.createElement('div');
  host.id = HOST_ID;
  const root = host.attachShadow({ mode: 'open' });
  const style = document.createElement('style');
  style.textContent = CSS;
  root.append(style);
  root.innerHTML += HTML;
  document.documentElement.append(host);

  const prefs = loadPrefs();
  // Declared up here because the voice picker's status handling has to know not to overwrite a
  // live transport message, and the voice list arrives asynchronously.
  let reading = false;
  let paused = false;
  let previewing = false;
  let bookOpen = initial.bookOpen;
  /**
   * Whether the voice list has arrived.
   *
   * Preview is gated on it, and it has to be: the picker holds a placeholder `Loading…` option
   * until then, so a press in that window samples no voice at all and has the narrator announce
   * itself as "Loading". That window is not small - answering `voices()` can mean starting the
   * native host and loading the model. It also stays false when the list arrives empty or the
   * request fails, which is right: there is nothing to preview.
   */
  let voicesReady = false;
  const $ = <T extends Element>(sel: string) => root.querySelector<T>(sel)!;
  const wrap = $<HTMLDivElement>('.wrap');
  const dot = $<HTMLSpanElement>('.dot');
  const statusEl = $<HTMLDivElement>('[data-el="status"]');
  const voiceEl = $<HTMLSelectElement>('[data-el="voice"]');
  const accentEl = $<HTMLSelectElement>('[data-el="accent"]');
  const genderEl = $<HTMLSelectElement>('[data-el="gender"]');
  const rateEl = $<HTMLInputElement>('[data-el="rate"]');
  const rateVal = $<HTMLSpanElement>('[data-el="rateval"]');
  const btn = (act: string) => $<HTMLButtonElement>(`[data-act="${act}"]`);

  const status: PanelHandle['status'] = (text, tone = 'idle') => {
    statusEl.textContent = text;
    statusEl.classList.toggle('error', tone === 'error');
    dot.className = `dot${tone === 'idle' ? '' : ` ${tone}`}`;
  };

  // --- fold
  if (prefs.folded) wrap.classList.add('folded');
  $<HTMLElement>('header').addEventListener('click', () => {
    prefs.folded = wrap.classList.toggle('folded');
    savePrefs(prefs);
  });

  // --- voices
  //
  // Three dependent dropdowns - accent, gender, name - rather than one list of every voice.
  // Thirty rows in a 260 px panel scrolls and hides most of itself; two filters cut the name
  // list to at most a dozen and put the part that decides how a voice SOUNDS in front of the
  // part that only names it.
  const esc = (s: string) =>
    s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

  const opt = (value: string, label: string, on: boolean, title = label) =>
    `<option value="${esc(value)}" title="${esc(title)}"${on ? ' selected' : ''}>${esc(label)}</option>`;

  let tree: AccentBucket[] = [];
  let remote = false;

  /**
   * The resting status line: which engine is speaking, or why Kokoro is not.
   *
   * Kokoro vs the platform voice is the difference the whole backend exists for, and it was
   * previously only discoverable in the service worker console.
   *
   * On the library the same line says why the transport is greyed out. That matters more here
   * than in the reader: a panel whose buttons are all dim and which says nothing about it looks
   * broken, and "the host is down" and "no book is open" are the same picture otherwise.
   */
  const showEngine = () => {
    const err = actions.engineError();
    if (err) status(`${actions.engine()} - Kokoro unavailable: ${err}`, 'error');
    else status(`${bookOpen ? 'Ready' : 'Open a book to read'} - ${actions.engine()}`);
  };

  /**
   * Say whether the CURRENT voice leaves the machine - including when it stops doing so.
   *
   * Setting the warning without ever clearing it left "page text is sent to the provider" on
   * screen after switching back to a local voice, which is a false claim about where the book
   * is going. Never overwrite a live transport status; that one is about playback, not privacy.
   */
  const noteVoice = () => {
    if (remote) status('Network voice: page text is sent to the provider.', 'error');
    else if (!reading && !previewing) showEngine();
  };

  /**
   * Repaint all three dropdowns around a wanted selection, resolving whatever is unavailable.
   *
   * Everything downstream falls back to the first entry of its parent, so no combination is ever
   * a dead end: switching accent while on "Female" keeps Female when that accent has one and
   * silently lands somewhere valid when it does not.
   */
  const render = (want: { accent?: string; gender?: string; name?: string }): void => {
    const accent = tree.find((a) => a.label === want.accent) ?? tree[0];
    if (!accent) return;
    const gender = accent.genders.find((g) => g.label === want.gender) ?? accent.genders[0]!;
    const voice = gender.voices.find((v) => v.name === want.name) ?? gender.voices[0]!;

    accentEl.innerHTML = tree.map((a) => opt(a.label, a.short, a === accent, a.label)).join('');
    genderEl.innerHTML = accent.genders.map((g) => opt(g.label, g.label, g === gender)).join('');
    // The row is the NAME alone. The list is in quality order (see `voices.ts`), but the grade is
    // not printed beside it: a column of "(A)"/"(C+)" is a second thing to read on every row of a
    // 260 px panel, and the order already puts the best one at the top, which is what the grade
    // was there to say.
    // The tooltip keeps the id - it is what the daemon and the console want - and the grade with
    // it, so the ordering is still explicable on hover without being in the way.
    voiceEl.innerHTML = gender.voices
      .map((v) =>
        opt(
          v.name,
          v.desc.name,
          v === voice,
          v.desc.quality ? `${v.name} - Kokoro grade ${v.desc.quality}` : v.name,
        ),
      )
      .join('');

    remote = voice.remote ?? false;
    prefs.voice = voice.name;
    savePrefs(prefs);
  };

  /**
   * Fetch the voice list and build the picker.
   *
   * A function rather than a one-shot, because failing here used to be permanent: `voicesReady`
   * stayed false, and with Play doubling as the library's preview that left the only control on
   * that surface disabled for the life of the panel - over a host that may have come back seconds
   * later. Retried on every surface change (see the handle's `setBookOpen`), which is the cheapest
   * honest trigger: it is the user having done something.
   */
  let loadingVoices = false;
  const loadVoices = async (): Promise<void> => {
    if (loadingVoices || voicesReady) return;
    loadingVoices = true;
    try {
      const vs = await actions.voices();
      if (!vs.length) {
        voiceEl.innerHTML = '<option value="">no voices found</option>';
        status('No TTS voice available on this system.', 'error');
        return;
      }
      const english = vs.filter((v) => !v.lang || v.lang.toLowerCase().startsWith('en'));
      tree = buildVoiceTree(english.length ? english : vs);

      // A "remote" voice (Chrome's bundled Google ones) synthesizes on a server, so the page
      // text leaves the machine - the one thing this tool promises not to do. Those sit under
      // their own accent entry, so they cannot be reached without asking for them.
      const at = locateVoice(tree, prefs.voice);
      render({ ...(at ?? {}), name: at ? prefs.voice : undefined });

      voicesReady = true;
      syncTransport();
      // `noteVoice` alone, not `showEngine()` first: this can land while a preview is playing,
      // and it is the one place that knows the resting line must not overwrite a live one.
      noteVoice();
    } catch (e) {
      voiceEl.innerHTML = '<option value="">unavailable</option>';
      status(`Voices: ${String(e)}`, 'error');
    } finally {
      loadingVoices = false;
    }
  };
  void loadVoices();

  for (const el of [accentEl, genderEl]) {
    el.addEventListener('change', () => {
      render({ accent: accentEl.value, gender: genderEl.value });
      noteVoice();
    });
  }
  voiceEl.addEventListener('change', () => {
    render({ accent: accentEl.value, gender: genderEl.value, name: voiceEl.value });
    noteVoice();
  });

  // --- speed
  /**
   * ONE options object for the session, MUTATED rather than replaced.
   *
   * Everything downstream re-reads `rate` as it sends each chunk, so writing to this object is what
   * carries a speed change into the page already playing. A fresh object per Play froze the speed
   * at the moment Play was pressed - for the whole book, since `readBook` passes what it was given
   * to every page - and that is what made the slider look dead.
   *
   * The voice is deliberately NOT live: it is set at Play and left alone, so switching voices takes
   * effect on the next Play rather than swapping narrator mid-sentence.
   */
  const live: SpeakOptions = { voiceName: prefs.voice || undefined, rate: prefs.rate };

  const opts = (): SpeakOptions => {
    live.voiceName = prefs.voice || undefined;
    live.rate = prefs.rate;
    return live;
  };

  rateEl.value = String(prefs.rate);
  rateVal.textContent = fmtRate(prefs.rate);

  // The readout follows the drag; the SPEED changes when the drag ends.
  //
  // `input` fires continuously - dozens of times across one drag - and each committed value costs a
  // real retune: the audio rendered ahead at the old speed is thrown away and asked for again, on
  // the single synth worker Kindle also queues behind. `change` is the same value a moment later
  // (drag release, or each keyboard step), and it is once.
  rateEl.addEventListener('input', () => {
    prefs.rate = Number(rateEl.value);
    rateVal.textContent = fmtRate(prefs.rate);
    savePrefs(prefs);
  });
  rateEl.addEventListener('change', () => {
    live.rate = prefs.rate;
    actions.retune(prefs.rate);
  });

  // --- transport
  //
  // Play and Resume are separate buttons, and their icons are near-identical by nature, so the
  // ENABLED state is what tells them apart: exactly one of the four is ever the obvious thing to
  // press. Play only while stopped, Pause only while speaking, Resume only while paused.
  //
  // PLAY IS TWO ACTIONS, chosen by whether a book is open. On the reader it reads from this page
  // on. On the library - where there is no page image to capture, so a read could only ever end in
  // an error message - it plays a sample of the selected voice instead, which is what the panel is
  // on the library FOR. One button rather than two, because it is one intention ("let me hear
  // it"), and because a fifth control that is dead on whichever surface you are looking at is
  // worse than four that always mean something.
  //
  // The glyph is the same for both, so `title`/`aria-label` are the only things that distinguish
  // reading a book from sampling a voice. They have to be kept accurate here.
  //
  // As a sample it needs a voice, so it waits for the list: "Loading…" is a real option in that
  // dropdown, and a press before the list lands samples no voice at all and has the narrator
  // announce itself as "Loading". As a read it does NOT wait - `readBook` asks for the voices
  // itself and reports what is wrong with them.
  //
  // Stop stays reachable whenever anything is speaking - a sample, or a book that has been closed
  // out from under the reader - because that is the one control that must never become unavailable
  // while audio is playing.
  //
  // A hoisted `function`, not the `const` arrow the other helpers here are: the voice list
  // resolving has to re-run this to release Play-as-preview, and that block is written above this
  // one.
  function syncTransport(): void {
    const play = btn('play');
    play.disabled = reading || previewing || (!bookOpen && !voicesReady);
    // Not "Loading voices…" for the waiting case: the same gate covers the list arriving EMPTY and
    // the request failing, and in both of those it has finished loading and there is still nothing
    // to play. The status line carries which of the three it is.
    play.title = bookOpen
      ? 'Read from this page on'
      : voicesReady
        ? 'Hear the selected voice'
        : 'No voice to preview yet';
    play.setAttribute('aria-label', bookOpen ? 'Play' : 'Preview voice');
    btn('pause').disabled = !reading || paused;
    btn('resume').disabled = !reading || !paused;
    btn('stop').disabled = !reading && !previewing;
  }

  const setReading = (on: boolean) => {
    reading = on;
    paused = false;
    syncTransport();
  };

  /**
   * Adopt a new surface. Shared by the 500 ms poll and by a press that found the cache stale, so
   * the two cannot drift into doing different things with the same fact.
   *
   * Also the retry point for a voice list that failed: a surface change is the user having done
   * something, and it is the cheapest honest trigger there is. Without it, one failed `voices()`
   * left the library's only control disabled for the life of the panel.
   */
  function applyBookOpen(open: boolean): void {
    if (open === bookOpen) return;
    bookOpen = open;
    syncTransport();
    void loadVoices();
    // Never over-write a live transport message ("Reading…", "Previewing…") or the network-voice
    // warning: closing a book says nothing about any of them. `noteVoice` owns that precedence.
    if (!reading && !previewing) noteVoice();
  }

  /**
   * Which press owns the panel.
   *
   * Stop does not settle the promise the press is awaiting - it signals the engine and returns -
   * so a stopped action's continuation arrives whenever the engine gets round to it, by which
   * time a NEW press may own the transport. Without this, that stale continuation clears the new
   * action's state: buttons back to idle, "Stopped."/resting line over the live one, and Stop
   * disabled while audio is still playing. Compare the token, don't trust the flag - the flag is
   * exactly what the newer press has already re-set for its own reasons.
   *
   * One counter for both actions, because either can supersede either: Stop mid-sample then a
   * book, or Stop mid-book then a sample.
   */
  let epoch = 0;

  /** Read from the page on screen onwards. Play, on the reader. */
  const runRead = async (): Promise<void> => {
    const mine = ++epoch;
    setReading(true);
    const pos = actions.position();
    status(`Reading${pos.page ? ` from page ${pos.page}` : ''}… (capturing + OCR)`, 'busy');
    try {
      await actions.readBook(opts());
      if (mine !== epoch) return;
      status(reading ? 'Finished.' : 'Stopped.');
    } catch (e) {
      if (mine !== epoch) return;
      status(String(e), 'error');
    } finally {
      if (mine === epoch) setReading(false);
    }
  };

  // The name shown in the dropdown, which is also what the sample says. Read at press time rather
  // than kept in a variable, so it is the voice on screen and not the one that was there when the
  // list was last rebuilt. Taken verbatim: the row is the name and nothing else, and a platform
  // voice can legitimately carry parentheses ("Microsoft David (Natural)") that are part of it.
  const selectedVoiceName = (): string => voiceEl.selectedOptions[0]?.textContent?.trim() || 'this voice';

  /** Say one sentence in the selected voice. Play, on the library. */
  const runPreview = async (): Promise<void> => {
    // The enable-gate cannot see an utterance the panel did not start - `kwr.readBook()` from the
    // console leaves every flag here at rest, and a sample started on top of one would not play
    // alongside it, it would end it. Asked at press time because nothing repaints the panel when
    // the console speaks, so this is a refusal rather than a dimmed button.
    if (actions.narrating()) {
      status('Something is already being read. Stop it first.', 'error');
      return;
    }

    const mine = ++epoch;
    const name = selectedVoiceName();
    previewing = true;
    syncTransport();
    status(`Previewing ${name}…`, 'busy');
    try {
      await actions.preview(sampleFor(name), opts());
      if (mine !== epoch) return;
      // Stop clears `previewing` before this resolves and puts its own message up. Cleared here
      // BEFORE `noteVoice`, which is what decides the resting line and refuses to overwrite a
      // sample in progress.
      const cancelled = !previewing;
      previewing = false;
      syncTransport();
      if (!cancelled) noteVoice();
    } catch (e) {
      if (mine !== epoch) return;
      previewing = false;
      syncTransport();
      status(String(e), 'error');
    }
  };

  // Which of the two this press is, decided at press time rather than latched at mount: the panel
  // outlives the book, so the same button is a preview on the library and a read once one is open.
  //
  // From a LIVE read, not the polled flag. `bookOpen` is refreshed on a 500 ms tick, so opening a
  // book and pressing Play inside that window would have sampled a voice instead of reading the
  // page already on screen - and then relabelled itself mid-action when the tick caught up. The
  // cache is brought forward first, so the label the press leaves behind describes what it did.
  btn('play').addEventListener('click', () => {
    const open = actions.hasBook();
    if (open !== bookOpen) applyBookOpen(open);
    void (open ? runRead() : runPreview());
  });

  btn('pause').addEventListener('click', () => {
    actions.pause();
    paused = true;
    syncTransport();
    status('Paused.');
  });

  btn('resume').addEventListener('click', () => {
    actions.resume();
    paused = false;
    syncTransport();
    status('Reading…', 'busy');
  });

  btn('stop').addEventListener('click', () => {
    actions.stop();
    previewing = false;
    setReading(false); // syncs the transport, so it runs after `previewing` is cleared
    status('Stopped.');
  });

  syncTransport();
  status(bookOpen ? 'Ready.' : 'Open a book to read.');

  return {
    destroy: () => host.remove(),
    status,
    setBookOpen: applyBookOpen,
  };
}
