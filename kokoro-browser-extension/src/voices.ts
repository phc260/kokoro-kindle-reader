// Turning a voice's NAME into something a person can choose from.
//
// Kokoro voice ids are a convention, not opaque strings: `am_fenrir` is (a)merican, (m)ale,
// "Fenrir". Twenty of those in one flat <select> is a wall of snake_case in which the first two
// characters - the only part that says how the voice will actually SOUND - are the easiest part
// to miss. Split into (accent, gender, name) the same list becomes four short groups.
//
// Platform voices (chrome.tts, speechSynthesis) do not follow the convention and their names are
// vendor prose: "Microsoft David - English (United States)". Everything here degrades to "use the
// name as given" for those rather than guessing, and only trims a suffix when the accent it
// repeats has already been established from `lang`.

import type { VoiceInfo } from './speak';

export type Gender = 'female' | 'male';

export interface VoiceDescription {
  /** The name alone: "Fenrir". Suitable as the <option> text inside an accent/gender group. */
  name: string;
  /** "American English" - absent when it cannot be told. */
  accent?: string;
  /** "American" - the same thing at the width a narrow dropdown affords. */
  accentShort?: string;
  gender?: Gender;
  /** BCP-47 tag implied by the id, for voices whose id implies one. */
  lang?: string;
  /** <optgroup> heading: "American English - Female". */
  group: string;
  /** Compact always-visible qualifier: "American - female". Empty when nothing is known. */
  detail: string;
}

interface Accent {
  full: string;
  short: string;
  lang: string;
}

/**
 * First letter of a Kokoro voice id.
 *
 * Only `a` and `b` ship here - espeak is pinned to `en-us`, so a non-English voice would be
 * phonemized as English and mispronounced. The rest are listed so that a voice someone drops
 * into the model directory is still *described* instead of shown raw; being able to read it is
 * not a claim that it will sound right.
 */
const BY_PREFIX: Record<string, Accent> = {
  a: { full: 'American English', short: 'American', lang: 'en-US' },
  b: { full: 'British English', short: 'British', lang: 'en-GB' },
  e: { full: 'Spanish', short: 'Spanish', lang: 'es-ES' },
  f: { full: 'French', short: 'French', lang: 'fr-FR' },
  h: { full: 'Hindi', short: 'Hindi', lang: 'hi-IN' },
  i: { full: 'Italian', short: 'Italian', lang: 'it-IT' },
  j: { full: 'Japanese', short: 'Japanese', lang: 'ja-JP' },
  p: { full: 'Brazilian Portuguese', short: 'Brazilian', lang: 'pt-BR' },
  z: { full: 'Mandarin Chinese', short: 'Mandarin', lang: 'zh-CN' },
};

/** Fallback for platform voices, which carry a BCP-47 tag but no gender and no naming rule. */
const BY_LANG: Record<string, Omit<Accent, 'lang'>> = {
  'en-us': { full: 'American English', short: 'American' },
  'en-gb': { full: 'British English', short: 'British' },
  'en-au': { full: 'Australian English', short: 'Australian' },
  'en-ca': { full: 'Canadian English', short: 'Canadian' },
  'en-ie': { full: 'Irish English', short: 'Irish' },
  'en-in': { full: 'Indian English', short: 'Indian' },
  'en-nz': { full: 'New Zealand English', short: 'New Zealand' },
  'en-za': { full: 'South African English', short: 'South African' },
};

/** `<lang><gender>_<name>`. Deliberately strict, so no platform voice can accidentally match. */
const KOKORO_ID = /^([a-z])([fm])_([a-z][a-z0-9]*)$/;

/** BCP-47 tag for a voice id, when the id says. Lets the Kokoro narrators stop claiming en-US
 *  for the British voices. */
export function langOf(name: string): string | undefined {
  const m = KOKORO_ID.exec(name);
  return m ? BY_PREFIX[m[1]!]?.lang : undefined;
}

export function describeVoice(name: string, lang?: string): VoiceDescription {
  const id = KOKORO_ID.exec(name);
  if (id) {
    const accent = BY_PREFIX[id[1]!];
    const bare = id[3]!;
    return format(
      bare.charAt(0).toUpperCase() + bare.slice(1),
      accent,
      id[2] === 'f' ? 'female' : 'male',
      accent?.lang ?? lang,
    );
  }

  const tag = lang?.toLowerCase();
  const known = tag ? BY_LANG[tag] : undefined;
  const accent: Accent | undefined = known ? { ...known, lang: lang! } : undefined;
  const tidied = tidy(name, !!accent);
  // An unrecognized tag is still worth showing - "de-DE" beats nothing at all.
  return format(tidied.name, accent ?? (lang ? { full: lang, short: lang, lang } : undefined), tidied.gender, lang);
}

function format(name: string, accent: Accent | undefined, gender: Gender | undefined, lang: string | undefined): VoiceDescription {
  const group = accent
    ? gender
      ? `${accent.full} - ${gender === 'female' ? 'Female' : 'Male'}`
      : accent.full
    : 'Other voices';
  const detail = [accent?.short, gender].filter(Boolean).join(' - ');
  return { name, accent: accent?.full, accentShort: accent?.short, gender, lang, group, detail };
}

/**
 * Best-effort cleanup of a vendor voice name.
 *
 * Two narrow rules, both safe because they only ever remove information that the group heading
 * is about to state anyway:
 *   - a trailing "Female"/"Male" word ("Google UK English Female"), which also yields the gender;
 *   - a trailing " - ..." or "(...)" language restatement, but ONLY when the accent is already
 *     known from `lang`, since that is what makes the suffix redundant rather than the only clue.
 */
function tidy(name: string, accentKnown: boolean): { name: string; gender?: Gender } {
  let out = name.trim();
  let gender: Gender | undefined;

  const g = /\b(fe)?male\b/i.exec(out);
  if (g) {
    gender = g[1] ? 'female' : 'male';
    out = out.replace(/[\s,]*\b(fe)?male\b\s*$/i, '').trim() || out;
  }

  if (accentKnown) {
    const shorter = out
      .replace(/\s*[-–—]\s+.*$/, '')
      .replace(/\s*\([^)]*\)\s*$/, '')
      .trim();
    if (shorter) out = shorter;
  }

  return { name: out, gender };
}

// ------------------------------------------------------------------------------- grouping

export type DescribedVoice = VoiceInfo & { desc: VoiceDescription };

export interface GenderBucket {
  /** "Female" | "Male" | "Any". */
  label: string;
  voices: DescribedVoice[];
}

export interface AccentBucket {
  /** "American English" - the full name, for tooltips. */
  label: string;
  /** "American" - what fits in a third of a 260 px panel. */
  short: string;
  genders: GenderBucket[];
}

/**
 * Network voices get their own bucket rather than a suffix. Choosing one sends the book's text
 * to a server, which is the single thing this project exists to avoid, so it should read as a
 * separate category and not as one more entry in the list. As an *accent* entry that means they
 * are unreachable until deliberately selected, which is stronger than a label on a row.
 */
export const NETWORK_GROUP = 'Network voices - text leaves your machine';
const NETWORK_SHORT = 'Network';

/** Bucket for a voice whose accent could not be told - a platform voice with an odd lang tag. */
const OTHER_ACCENT = 'Other';

/** Gender bucket for voices that do not state one. Not "Unknown": as a filter it means "all". */
export const ANY_GENDER = 'Any';

const ACCENT_ORDER = ['American English', 'British English'];
const GENDER_LABELS = ['Female', 'Male', ANY_GENDER];

const accentRank = (d: VoiceDescription): number => {
  const i = ACCENT_ORDER.indexOf(d.accent ?? '');
  if (i >= 0) return i;
  return d.accent ? ACCENT_ORDER.length : ACCENT_ORDER.length + 1; // named accents before none
};

const genderRank = (label: string): number => {
  const i = GENDER_LABELS.indexOf(label);
  return i >= 0 ? i : GENDER_LABELS.length;
};

/**
 * The voice list as accent -> gender -> names, which is what the picker offers as three
 * dependent dropdowns.
 *
 * One flat list of every voice is ~30 rows and scrolls; the same voices behind two filters are
 * never more than a dozen. The tree only ever contains buckets that actually have voices in
 * them, so a gender with nothing under it cannot be selected and no combination is a dead end.
 *
 * Order: accent by preference (American, British, other, network last), then Female, Male, Any,
 * then name.
 */
export function buildVoiceTree(vs: readonly VoiceInfo[]): AccentBucket[] {
  // The bucket a voice lands in is decided FIRST, because it is not always its own accent:
  // every remote voice collapses into one network bucket regardless of where it is from.
  const placed = vs.map((v) => {
    const desc = describeVoice(v.name, v.lang);
    const remote = v.remote ?? false;
    return {
      voice: { ...v, desc } as DescribedVoice,
      label: remote ? NETWORK_GROUP : (desc.accent ?? OTHER_ACCENT),
      short: remote ? NETWORK_SHORT : (desc.accentShort ?? OTHER_ACCENT),
      gender: desc.gender ? (desc.gender === 'female' ? 'Female' : 'Male') : ANY_GENDER,
    };
  });

  placed.sort((a, b) => {
    const remote = Number(a.voice.remote ?? false) - Number(b.voice.remote ?? false);
    if (remote) return remote;
    // Ordering by the *underlying* accent is meaningless once several of them have collapsed
    // into the single network bucket - and it is what used to interleave them.
    if (!a.voice.remote) {
      const byRank = accentRank(a.voice.desc) - accentRank(b.voice.desc);
      if (byRank) return byRank;
      const byLabel = a.label.localeCompare(b.label);
      if (byLabel) return byLabel;
    }
    return genderRank(a.gender) - genderRank(b.gender) || a.voice.desc.name.localeCompare(b.voice.desc.name);
  });

  // Keyed lookup, never adjacency. Sorting on one key and then merging neighbours on a
  // different key produced duplicate buckets - two "Female" entries under Network, of which the
  // panel could only ever reach the first, silently swapping a saved voice for another.
  const byLabel = new Map<string, AccentBucket>();
  const out: AccentBucket[] = [];
  for (const p of placed) {
    let accent = byLabel.get(p.label);
    if (!accent) {
      accent = { label: p.label, short: p.short, genders: [] };
      byLabel.set(p.label, accent);
      out.push(accent);
    }

    let bucket = accent.genders.find((g) => g.label === p.gender);
    if (!bucket) accent.genders.push((bucket = { label: p.gender, voices: [] }));

    bucket.voices.push(p.voice);
  }
  return out;
}

/** Which two filters land on `name`, so a saved voice can restore all three dropdowns. */
export function locateVoice(tree: readonly AccentBucket[], name: string): { accent: string; gender: string } | null {
  for (const a of tree) for (const g of a.genders) if (g.voices.some((v) => v.name === name)) return { accent: a.label, gender: g.label };
  return null;
}
