// The voice list is the one piece of UI a reader touches on every session, and its labels are
// derived from strings the backend and the browser hand us. The rules that matter: a Kokoro id
// always decomposes, a platform voice is never mangled by trying, and a network voice never
// hides among the local ones.

import { test, expect } from 'bun:test';
import {
  describeVoice,
  buildVoiceTree,
  locateVoice,
  langOf,
  qualityOf,
  NETWORK_GROUP,
  ANY_GENDER,
} from '../src/voices';
import type { VoiceInfo } from '../src/speak';

/** The voices the daemon actually reports, as seen in the panel. */
const KOKORO = [
  'af_alloy', 'af_aoede', 'af_bella', 'af_heart', 'af_jessica', 'af_kore',
  'af_nicole', 'af_nova', 'af_river', 'af_sarah', 'af_sky',
  'am_adam', 'am_echo', 'am_eric', 'am_fenrir', 'am_liam', 'am_michael',
  'am_onyx', 'am_puck', 'am_santa',
  'bf_alice', 'bf_emma', 'bf_isabella', 'bf_lily',
  'bm_daniel', 'bm_fable', 'bm_george', 'bm_lewis',
];

const local = (names: string[]): VoiceInfo[] => names.map((name) => ({ name, lang: langOf(name), remote: false }));

test('a Kokoro id splits into accent, gender and name', () => {
  expect(describeVoice('am_fenrir')).toMatchObject({
    name: 'Fenrir',
    accent: 'American English',
    gender: 'male',
    lang: 'en-US',
    group: 'American English - Male',
    detail: 'American - male',
  });
  expect(describeVoice('bf_alice')).toMatchObject({
    name: 'Alice',
    accent: 'British English',
    gender: 'female',
    lang: 'en-GB',
  });
});

test('every shipped voice decomposes - none falls through to the raw id', () => {
  for (const id of KOKORO) {
    const d = describeVoice(id);
    expect(d.name).not.toBe(id);
    expect(d.accent).toBeDefined();
    expect(d.gender).toBeDefined();
    expect(d.name[0]).toBe(d.name[0]!.toUpperCase());
  }
});

test('the British voices stop claiming to be American', () => {
  // kokoro.ts and kokoro-http.ts both used to hardcode en-US for the whole list.
  expect(langOf('bm_george')).toBe('en-GB');
  expect(langOf('af_heart')).toBe('en-US');
  expect(langOf('Microsoft David')).toBeUndefined();
});

test('a platform voice is described, never re-spelled', () => {
  // The id pattern must not match vendor prose, or the name would be destroyed.
  const d = describeVoice('Microsoft David - English (United States)', 'en-US');
  expect(d.name).toBe('Microsoft David');
  expect(d.accent).toBe('American English');
  expect(d.gender).toBeUndefined();
});

test('a redundant suffix is only trimmed when the accent is already known', () => {
  // Without a recognized lang the suffix is the ONLY clue about the voice, so it stays.
  const unknown = describeVoice('Some Voice - Klingon (Homeworld)', 'tlh');
  expect(unknown.name).toBe('Some Voice - Klingon (Homeworld)');
  expect(unknown.detail).toBe('tlh');
});

test('a gender stated in the name is read out of it', () => {
  const d = describeVoice('Google UK English Female', 'en-GB');
  expect(d.gender).toBe('female');
  expect(d.name).toBe('Google UK English');
  expect(d.group).toBe('British English - Female');
});

test('a nameless-after-trim voice keeps its original name', () => {
  expect(describeVoice('Female', 'en-US').name).toBe('Female');
});

test('the tree comes out in reading order: American before British, female before male', () => {
  const tree = buildVoiceTree(local(KOKORO));
  expect(tree.map((a) => [a.short, a.genders.map((g) => g.label)])).toEqual([
    ['American', ['Female', 'Male']],
    ['British', ['Female', 'Male']],
  ]);
  expect(tree[0]!.label).toBe('American English'); // full name still available for the tooltip
});

test('the tree loses no voice', () => {
  const tree = buildVoiceTree(local(KOKORO));
  const flat = tree.flatMap((a) => a.genders.flatMap((g) => g.voices.map((v) => v.name)));
  expect(flat.slice().sort()).toEqual(KOKORO.slice().sort());
});

test('a bucket is ordered by quality, best first - not alphabetically', () => {
  // The whole point: `af_alloy` sorts first by name and is a C; `af_heart` is the A in the
  // download. An unset preference lands on the first entry, so this decides which voice a reader
  // hears before they have chosen one.
  const tree = buildVoiceTree(local(KOKORO));
  const bucket = (accent: string, gender: string) =>
    tree.find((a) => a.label === accent)!.genders.find((g) => g.label === gender)!.voices.map((v) => v.name);

  expect(bucket('American English', 'Female')[0]).toBe('af_heart');
  expect(bucket('American English', 'Female').slice(0, 3)).toEqual(['af_heart', 'af_bella', 'af_nicole']);
  expect(bucket('British English', 'Female')[0]).toBe('bf_emma');
  // Equal grades fall back to the name, which is what keeps the order stable between sessions.
  expect(bucket('American English', 'Male').slice(0, 3)).toEqual(['am_fenrir', 'am_michael', 'am_puck']);
  // The bottom of the list is where an alphabetical order hurt least and hid most.
  expect(bucket('American English', 'Male').at(-1)).toBe('am_adam');
});

test('every bucket is in non-increasing grade order, with the ungraded last', () => {
  const rank = (name: string) => {
    const q = qualityOf(name);
    return q ? ['A', 'A-', 'B+', 'B', 'B-', 'C+', 'C', 'C-', 'D+', 'D', 'D-', 'F+', 'F'].indexOf(q) : 99;
  };
  const tree = buildVoiceTree([...local(KOKORO), { name: 'Microsoft David', lang: 'en-US' }]);
  for (const a of tree) {
    for (const g of a.genders) {
      const ranks = g.voices.map((v) => rank(v.name));
      expect(ranks).toEqual(ranks.slice().sort((x, y) => x - y));
    }
  }
});

test('a voice with no published grade is not ranked as the worst one', () => {
  // An ungraded voice is unknown, not bad - but it cannot jump the graded ones either, so it
  // sorts after them and keeps its alphabetical order among its own kind.
  expect(qualityOf('af_heart')).toBe('A');
  expect(qualityOf('Microsoft David')).toBeUndefined();
  expect(describeVoice('Microsoft David', 'en-US').quality).toBeUndefined();

  const tree = buildVoiceTree([
    { name: 'Zoe Platform', lang: 'en-US' },
    { name: 'Aaron Platform', lang: 'en-US' },
  ]);
  expect(tree[0]!.genders[0]!.voices.map((v) => v.name)).toEqual(['Aaron Platform', 'Zoe Platform']);
});

test('the grade travels with the description, so the picker can show it', () => {
  expect(describeVoice('af_bella')).toMatchObject({ name: 'Bella', quality: 'A-' });
  expect(describeVoice('am_adam').quality).toBe('F+');
});

test('every shipped voice has a grade', () => {
  // A voice added to the model dir without one degrades gracefully (it sorts last), but every
  // voice in the shipped set is in the published table and should stay listed here.
  for (const id of KOKORO) expect(qualityOf(id)).toBeDefined();
});

test('no bucket is empty, so no filter combination is a dead end', () => {
  for (const a of buildVoiceTree(local(KOKORO))) {
    expect(a.genders.length).toBeGreaterThan(0);
    for (const g of a.genders) expect(g.voices.length).toBeGreaterThan(0);
  }
});

test('a saved voice can be located, which is what restores all three dropdowns', () => {
  const tree = buildVoiceTree(local(KOKORO));
  expect(locateVoice(tree, 'bm_george')).toEqual({ accent: 'British English', gender: 'Male' });
  expect(locateVoice(tree, 'af_heart')).toEqual({ accent: 'American English', gender: 'Female' });
  // A voice that has gone away (engine switched, model dir changed) must not throw.
  expect(locateVoice(tree, 'nope')).toBeNull();
});

test('network voices are their own accent, last, and never reachable by accident', () => {
  const tree = buildVoiceTree([
    { name: 'Google US English', lang: 'en-US', remote: true },
    ...local(['af_heart', 'bm_george']),
  ]);
  expect(tree.at(-1)!.label).toBe(NETWORK_GROUP);
  expect(tree.at(-1)!.short).toBe('Network');
  // Crucially: no remote voice appears under a real accent, so selecting American/Female can
  // never hand you one.
  for (const a of tree.slice(0, -1)) {
    for (const g of a.genders) expect(g.voices.every((v) => !v.remote)).toBe(true);
  }
});

test('remote voices from several accents collapse into ONE bucket per gender', () => {
  // Found by an independent review. Remote voices were sorted by their underlying accent and
  // then relabelled to the single network bucket, so they arrived non-contiguous and the
  // adjacency merge emitted Female, Male, Female, Male. The panel finds a gender bucket by
  // label and takes the first match, so the later duplicates were unreachable - and a saved
  // preference for one of them silently restored a different voice.
  const tree = buildVoiceTree([
    { name: 'US Female', lang: 'en-US', remote: true },
    { name: 'US Male', lang: 'en-US', remote: true },
    { name: 'UK Female', lang: 'en-GB', remote: true },
    { name: 'UK Male', lang: 'en-GB', remote: true },
  ]);

  expect(tree).toHaveLength(1);
  const labels = tree[0]!.genders.map((g) => g.label);
  expect(labels).toEqual([...new Set(labels)]); // no duplicates, whatever the order
  expect(tree[0]!.genders.find((g) => g.label === 'Female')!.voices.map((v) => v.name)).toEqual([
    'UK Female',
    'US Female',
  ]);

  // The real consequence: every voice must be reachable through the filters that locate it.
  for (const name of ['US Female', 'US Male', 'UK Female', 'UK Male']) {
    const at = locateVoice(tree, name)!;
    const bucket = tree.find((a) => a.label === at.accent)!.genders.find((g) => g.label === at.gender)!;
    expect(bucket.voices.some((v) => v.name === name)).toBe(true);
  }
});

test('no accent label is ever duplicated either', () => {
  const tree = buildVoiceTree([
    ...local(['af_heart', 'bm_george']),
    { name: 'Some Voice', lang: 'en-US', remote: true },
    ...local(['af_bella']),
  ]);
  const labels = tree.map((a) => a.label);
  expect(labels).toEqual([...new Set(labels)]);
});

test('a voice with no stated gender gets a filter that means "all of them"', () => {
  const tree = buildVoiceTree([{ name: 'espeak' }]);
  expect(tree).toHaveLength(1);
  expect(tree[0]!.short).toBe('Other');
  expect(tree[0]!.genders[0]!.label).toBe(ANY_GENDER);
  expect(tree[0]!.genders[0]!.voices[0]!.desc.name).toBe('espeak');
});
