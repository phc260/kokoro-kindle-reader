// Curated list of Kokoro English voices the app offers in Settings. These voice
// packs are downloaded into the app data dir on first run and served over the
// `kokoro://` scheme, so switching voices works fully offline. IDs follow
// Kokoro's convention: first letter is language (a = American, b = British
// English), second is gender (f = female, m = male).
//
// NOTE: this list must stay in sync with the voice entries in
// src-tauri/model-manifest.json, which decides exactly which voice files get
// downloaded (and carries their SHA-256 for integrity verification).

export interface Voice {
  id: string;
  name: string;
  group: string;
}

export const DEFAULT_VOICE = "af_heart";

export const VOICES: Voice[] = [
  // American — female
  { id: "af_heart", name: "Heart", group: "American — Female" },
  { id: "af_bella", name: "Bella", group: "American — Female" },
  { id: "af_nicole", name: "Nicole", group: "American — Female" },
  { id: "af_aoede", name: "Aoede", group: "American — Female" },
  { id: "af_kore", name: "Kore", group: "American — Female" },
  { id: "af_sarah", name: "Sarah", group: "American — Female" },
  { id: "af_nova", name: "Nova", group: "American — Female" },
  { id: "af_sky", name: "Sky", group: "American — Female" },
  { id: "af_alloy", name: "Alloy", group: "American — Female" },
  { id: "af_jessica", name: "Jessica", group: "American — Female" },
  { id: "af_river", name: "River", group: "American — Female" },
  // American — male
  { id: "am_adam", name: "Adam", group: "American — Male" },
  { id: "am_michael", name: "Michael", group: "American — Male" },
  { id: "am_echo", name: "Echo", group: "American — Male" },
  { id: "am_eric", name: "Eric", group: "American — Male" },
  { id: "am_fenrir", name: "Fenrir", group: "American — Male" },
  { id: "am_liam", name: "Liam", group: "American — Male" },
  { id: "am_onyx", name: "Onyx", group: "American — Male" },
  { id: "am_puck", name: "Puck", group: "American — Male" },
  // British — female
  { id: "bf_emma", name: "Emma", group: "British — Female" },
  { id: "bf_alice", name: "Alice", group: "British — Female" },
  { id: "bf_isabella", name: "Isabella", group: "British — Female" },
  { id: "bf_lily", name: "Lily", group: "British — Female" },
  // British — male
  { id: "bm_george", name: "George", group: "British — Male" },
  { id: "bm_daniel", name: "Daniel", group: "British — Male" },
  { id: "bm_fable", name: "Fable", group: "British — Male" },
  { id: "bm_lewis", name: "Lewis", group: "British — Male" },
];

const VALID_IDS = new Set(VOICES.map((v) => v.id));

const BY_ID = new Map(VOICES.map((v) => [v.id, v]));

/** Read the persisted voice, falling back to the default if unset/unknown. */
export function loadVoice(): string {
  const saved = localStorage.getItem("tts-voice");
  return saved && VALID_IDS.has(saved) ? saved : DEFAULT_VOICE;
}

/** A short self-introduction spoken as the preview sample for a voice. Kept to
 * one sentence so it synthesizes quickly even on the WASM fallback. */
export function voiceIntro(id: string): string {
  const v = BY_ID.get(id);
  if (!v) return "Hi, I'd be glad to read the Scriptures to you.";
  const accent = v.group.startsWith("American") ? "American" : "British";
  return `Hi, I'm ${v.name}, your ${accent} reader. I'd be glad to read the Scriptures to you.`;
}
