// When each word of a chunk is heard. The only source of word boundaries on the Kokoro path.
//
// The platform engines report boundaries themselves (`chrome.tts`'s `word` event,
// `speechSynthesis`'s `onboundary`). Kokoro does not: `POST /synth` hands back a block of f32
// PCM and nothing else. The model *does* predict a duration per phoneme internally, but the
// stock `model.onnx` exposes only the waveform output (kokoro-host/src/native_synth.rs), so
// there is no alignment to ask for - it would take a re-exported model to expose one.
//
// What IS known exactly is the chunk's duration: the sample count is right there in the
// response. So this estimates only the SPLIT of that duration across the chunk's words, and the
// error resets to zero at every chunk boundary - one to four sentences, never a whole page.
// That bound is what makes an estimate good enough to highlight with.
//
// The weights are a syllable count, not a character count. "through" and "greenery" are the same
// length and take very different times to say; vowel groups track the difference well enough
// that the highlight stays on the right word within a line.

/** One word of a chunk, and how long it should take relative to its neighbours. */
export interface WordSpan {
  /** Offset within the chunk text. */
  charIndex: number;
  charLength: number;
  /** Relative duration. Unitless - only the ratios matter. */
  weight: number;
}

/** A word span placed on the clock, `at` seconds after the chunk's audio starts. */
export interface WordMark {
  charIndex: number;
  charLength: number;
  at: number;
}

/**
 * Fixed cost per word, on top of its syllables: the attack of the first consonant and the gap
 * before the next word, neither of which scales with length. Without it a one-syllable word
 * gets a quarter of the time a four-syllable one does, and the highlight sprints through
 * "of the" and then sits waiting.
 */
const WORD_OVERHEAD = 0.45;

/** Extra beats for the pause a mark introduces, in syllable-equivalents. */
const PAUSE = { clause: 0.5, sentence: 1.2, dash: 0.6 };

/**
 * Syllables in one word, by vowel groups.
 *
 * The classic heuristic, and wrong often enough to say so: "fire" scores 1, "poem" scores 1,
 * every silent-e rule has exceptions. It only has to rank words against each other within a
 * couple of sentences, which it does.
 */
export function syllables(word: string): number {
  const core = word.toLowerCase().replace(/[^a-z0-9]/g, '');
  if (!core) return 1;

  // Digits are spoken, not spelled: a bare "1997" is five syllables, not the one its zero vowel
  // groups would score. One per digit is closer than anything else this cheap.
  const digits = (core.match(/\d/g) ?? []).length;
  const letters = core.replace(/\d/g, '');
  if (!letters) return Math.max(1, digits);

  let n = (letters.match(/[aeiouy]+/g) ?? []).length;
  // Silent terminal e ("time", "spoke") - but never when it is the only vowel group ("the",
  // "she"), which would leave the word at zero.
  if (n > 1 && /[^aeiouy]e$/.test(letters)) n--;
  return Math.max(1, n) + digits;
}

/**
 * Every word of `text`, with its weight. Punctuation stays attached to the word it follows, so
 * the pause after a comma is charged to the word before it - which is where the ear puts it.
 */
export function wordSpans(text: string): WordSpan[] {
  const out: WordSpan[] = [];
  for (const m of text.matchAll(/\S+/g)) {
    const raw = m[0];
    let weight = syllables(raw) + WORD_OVERHEAD;
    // Trailing quotes and brackets are silent; look through them for the mark that is not.
    const tail = raw.replace(/["'”’)\]]+$/, '');
    if (/[.!?…]$/.test(tail)) weight += PAUSE.sentence;
    else if (/[,;:]$/.test(tail)) weight += PAUSE.clause;
    else if (/[-–—]$/.test(tail)) weight += PAUSE.dash;
    out.push({ charIndex: m.index, charLength: raw.length, weight });
  }
  return out;
}

/**
 * Place a chunk's words on the clock, given how long its audio actually is.
 *
 * Each mark is when the word STARTS, so a caller can move a highlight on it directly.
 */
export function scheduleWords(text: string, durationS: number): WordMark[] {
  const spans = wordSpans(text);
  const total = spans.reduce((n, s) => n + s.weight, 0);
  if (!spans.length || total <= 0 || !(durationS > 0)) return [];

  const out: WordMark[] = [];
  let acc = 0;
  for (const s of spans) {
    out.push({ charIndex: s.charIndex, charLength: s.charLength, at: (acc / total) * durationS });
    acc += s.weight;
  }
  return out;
}
