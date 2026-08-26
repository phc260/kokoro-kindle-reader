// SPDX-License-Identifier: MIT AND Apache-2.0
//
// Mixed licence: this file is MIT (see LICENSE) except for `phonemize_segment_spans`
// below, whose structure is ported from kokoro-js's `PhonemizeSegment`
// (https://github.com/hexgrad/kokoro, npm `kokoro-js`), licensed under the Apache
// License, Version 2.0 — trace a segment to a file, fold clause-per-line into one
// space-joined string. Modified for Kokoro Kindle Reader: reached over an FFI to
// espeak-ng.dll and extended to collect PHONEME events for word timing (the timing
// path is original and not derived from kokoro-js). Full licence text:
// licenses/Apache-2.0.txt. See THIRD_PARTY_NOTICES.md for the complete list of files
// this notice covers.
//
// espeak-ng FFI + one-segment phoneme trace (the phonemizer path kokoro-js uses via
// espeak). espeak keeps global state and isn't thread-safe — single worker only. The
// phoneme trace goes to a FILE*, so we FFI the CRT's fopen/fclose to feed it one.
//
// Alongside the trace we collect espeak's PHONEME events, which carry the text position of
// the WORD each phoneme belongs to. That is what lets a phoneme — and so a model-predicted
// duration — be attributed to a source word instead of to a character fraction. The trace
// remains the sole source of the phoneme STRING; events only supply the mapping. Keeping
// those two jobs apart is deliberate: reconstructing the string from event names would put
// the narrator's sound at the mercy of an event stream we only sanity-check, and adding
// timing must not change what the voice says.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::text::Span;

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum FILE {}

// espeak constants (from speak_lib.h / espeak_ng.h).
const ENOUTPUT_MODE_SYNCHRONOUS: c_int = 0x0001;
const POS_CHARACTER: c_int = 1;
const ESPEAK_CHARS_UTF8: c_uint = 1;
const ESPEAK_PHONEMES: c_uint = 0x100;
const ENS_OK: c_int = 0;
const EE_OK: c_int = 0;

const EV_LIST_TERMINATED: c_int = 0;
const EV_PHONEME: c_int = 7;

/// `espeak_EVENT` (speak_lib.h). The union is read as `string` — the only member the
/// PHONEME events we collect use.
#[repr(C)]
#[derive(Clone, Copy)]
struct EspeakEvent {
    etype: c_int,
    unique_identifier: c_uint,
    /// **1-based, in CHARACTERS** — not bytes. Confirmed against multi-byte input; a
    /// pipeline that scans UTF-8 bytes everywhere else has to convert here or it indexes
    /// into the middle of a character the moment a page contains an accent.
    text_position: c_int,
    length: c_int,
    audio_position: c_int,
    sample: c_int,
    user_data: *mut c_void,
    id: [u8; 8],
}

type SynthCallback = extern "C" fn(*mut i16, c_int, *mut EspeakEvent) -> c_int;

extern "C" {
    fn espeak_ng_InitializePath(path: *const c_char);
    fn espeak_ng_Initialize(context: *mut *mut c_void) -> c_int;
    fn espeak_ng_InitializeOutput(output_mode: c_int, buffer_length: c_int, device: *const c_char) -> c_int;
    fn espeak_ng_SetPhonemeEvents(enable: c_int, ipa: c_int);
    fn espeak_SetSynthCallback(cb: SynthCallback);
    fn espeak_SetVoiceByName(name: *const c_char) -> c_int;
    fn espeak_SetPhonemeTrace(phonememode: c_int, stream: *mut FILE);
    fn espeak_Synth(
        text: *const c_void,
        size: usize,
        position: c_uint,
        position_type: c_int,
        end_position: c_uint,
        flags: c_uint,
        unique_identifier: *mut c_uint,
        user_data: *mut c_void,
    ) -> c_int;

    // CRT FILE handling for the phoneme trace.
    fn fopen(path: *const c_char, mode: *const c_char) -> *mut FILE;
    fn fflush(f: *mut FILE) -> c_int;
    fn fclose(f: *mut FILE) -> c_int;
}

/// One collected PHONEME event: its IPA name and the 1-based character position of the
/// word it belongs to.
struct PhonEvent {
    name: Vec<u8>,
    text_position: c_int,
}

/// Events from the synth call in progress. espeak is single-worker by construction (it has
/// global state), and in SYNCHRONOUS mode the callback runs on the calling thread, so this
/// is only ever touched by one thread at a time — the Mutex is here to make that safe
/// rather than merely true.
static EVENTS: Mutex<Vec<PhonEvent>> = Mutex::new(Vec::new());

extern "C" fn collect(_wav: *mut i16, _n: c_int, events: *mut EspeakEvent) -> c_int {
    // Audio is discarded; we only want the phoneme trace and the positions.
    if events.is_null() {
        return 0;
    }
    let mut out = match EVENTS.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    unsafe {
        let mut p = events;
        loop {
            let e = *p;
            if e.etype == EV_LIST_TERMINATED {
                break;
            }
            if e.etype == EV_PHONEME {
                let end = e.id.iter().position(|&b| b == 0).unwrap_or(e.id.len());
                if end > 0 {
                    out.push(PhonEvent {
                        name: e.id[..end].to_vec(),
                        text_position: e.text_position,
                    });
                }
            }
            p = p.add(1);
        }
    }
    0
}

/// One-time espeak init (SYNCHRONOUS output so Synth blocks; audio discarded).
pub fn init(espeak_data_dir: &str) -> Result<(), String> {
    let dir = CString::new(espeak_data_dir).map_err(|_| "bad data dir".to_string())?;
    unsafe {
        espeak_ng_InitializePath(dir.as_ptr());
        let mut ctx: *mut c_void = ptr::null_mut();
        if espeak_ng_Initialize(&mut ctx) != ENS_OK {
            return Err("espeak_ng_Initialize failed".into());
        }
        if espeak_ng_InitializeOutput(ENOUTPUT_MODE_SYNCHRONOUS, 0, ptr::null()) != ENS_OK {
            return Err("espeak_ng_InitializeOutput failed".into());
        }
        // Phoneme events, IPA names — the source of the phoneme-to-word mapping. Verified
        // not to alter the phoneme trace itself: the same text traces identically with
        // these on and off.
        espeak_ng_SetPhonemeEvents(1, 1);
        espeak_SetSynthCallback(collect);
        let voice = CString::new("en-us").unwrap();
        if espeak_SetVoiceByName(voice.as_ptr()) != EE_OK {
            return Err("espeak_SetVoiceByName(en-us) failed".into());
        }
    }
    Ok(())
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A segment's phonemes plus, per phoneme byte, the range of the SEGMENT's bytes that
/// produced it.
pub struct Phonemes {
    pub text: Vec<u8>,
    pub spans: Vec<Span>,
}

// A UTF-8 codepoint's byte length from its lead byte.
fn utf8_len(c: u8) -> usize {
    if c < 0x80 { 1 } else if (c >> 5) == 0x6 { 2 } else if (c >> 4) == 0xE { 3 } else if (c >> 3) == 0x1E { 4 } else { 1 }
}

/// espeak synth-trace phonemization of one punctuation-free UTF-8 segment, plus the
/// phoneme-to-source mapping.
///
/// Mirrors PhonemizeSegment: trace to a temp FILE, then fold clause-per-line into a single
/// space-joined string. The trace decides that string and nothing here can change it — adding
/// the spans did not move a byte of it. `spans` is best-effort:
/// if the event stream cannot be aligned to the trace it degrades to one span covering the
/// whole segment, which is the granularity the host had before any of this. Degrading is
/// the right failure here because the alternative — attributing phonemes to whichever word
/// the walk had drifted onto — would still produce marks, and they would be wrong in a way
/// nothing downstream could detect.
pub fn phonemize_segment_spans(text: &[u8]) -> Phonemes {
    let whole = Span { start: 0, end: text.len() as u32 };
    if let Ok(mut g) = EVENTS.lock() {
        g.clear();
    }

    // unique temp path
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("kokoro_phon_{pid}_{n}.txt"));
    let tmp_c = match CString::new(tmp.to_string_lossy().to_string()) {
        Ok(c) => c,
        Err(_) => return Phonemes { text: Vec::new(), spans: Vec::new() },
    };
    let mode = CString::new("wb+").unwrap();

    // text must be NUL-terminated; size includes the terminator.
    let text_c = match CString::new(text) {
        Ok(c) => c,
        Err(_) => return Phonemes { text: Vec::new(), spans: Vec::new() },
    };
    let bytes_with_nul = text_c.as_bytes_with_nul();

    unsafe {
        let f = fopen(tmp_c.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Phonemes { text: Vec::new(), spans: Vec::new() };
        }
        espeak_SetPhonemeTrace(0x02, f); // bit1 = IPA
        espeak_Synth(
            bytes_with_nul.as_ptr() as *const c_void,
            bytes_with_nul.len(),
            0,
            POS_CHARACTER,
            0,
            ESPEAK_CHARS_UTF8 | ESPEAK_PHONEMES,
            ptr::null_mut(),
            ptr::null_mut(),
        );
        espeak_SetPhonemeTrace(0x02, ptr::null_mut());
        fflush(f);
        fclose(f);
    }

    let buf = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);

    // one clause per line -> join with a single space
    let mut p: Vec<u8> = Vec::new();
    for &ch in &buf {
        if ch == b'\n' || ch == b'\r' || ch == b'\t' {
            if !p.is_empty() && *p.last().unwrap() != b' ' {
                p.push(b' ');
            }
        } else {
            p.push(ch);
        }
    }
    while !p.is_empty() && *p.last().unwrap() == b' ' {
        p.pop();
    }

    let events = EVENTS.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
    let spans = attribute(text, &p, &events).unwrap_or_else(|| vec![whole; p.len()]);
    Phonemes { text: p, spans }
}

/// Map each phoneme byte of `p` to the segment bytes that produced it, or `None` if the
/// event stream and the trace disagree.
///
/// espeak's word reporting is **not a clean partition of the input** and must not be used
/// as one. Observed on real text: a token can raise two WORD events with overlapping
/// positions (`77` -> "seventy seven"), a hyphenated compound raises one event for its
/// first part only (`state-of-the-art`), a character espeak expands to a phrase raises an
/// event for a following space, and trailing phonemes carry a position past the end of the
/// input. The reported `length` is what goes wrong in every one of those cases, so it is
/// not used at all.
///
/// What IS reliable is the position each phoneme is stamped with: it is the start of the
/// word that phoneme came from. So word extents are rebuilt by TILING — word `k` runs from
/// its own start to the next distinct start — which is monotonic by construction, covers
/// every byte of the segment, and gives a compound a single span covering the whole of it.
/// That is also the right answer for a highlight, since a compound is one word on the page.
fn attribute(text: &[u8], p: &[u8], events: &[PhonEvent]) -> Option<Vec<Span>> {
    if p.is_empty() {
        return Some(Vec::new());
    }
    if events.is_empty() {
        return None;
    }

    // 1-based character position -> byte offset in the segment.
    let mut char_byte: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        char_byte.push(i);
        i += utf8_len(text[i]);
    }
    char_byte.push(text.len());
    let nchars = char_byte.len() - 1;

    // Distinct, monotonic word starts (0-based chars), always covering from the top of the
    // segment so leading text is never orphaned.
    let mut starts: Vec<usize> = vec![0];
    for e in events {
        let c = (e.text_position.max(1) as usize - 1).min(nchars);
        if c > *starts.last().unwrap() {
            starts.push(c);
        }
    }

    let span_at = |pos: c_int| -> Span {
        let c = (pos.max(1) as usize - 1).min(nchars);
        // The last word start at or before this position.
        let k = match starts.binary_search(&c) {
            Ok(k) => k,
            Err(k) => k.saturating_sub(1),
        };
        let a = char_byte[starts[k]];
        let b = char_byte[starts.get(k + 1).copied().unwrap_or(nchars)];
        Span { start: a as u32, end: b as u32 }
    };

    // Walk the trace and the events together. A phoneme event's name must be the next
    // non-space run of the trace; anything else means the two have drifted apart and the
    // mapping is void.
    let mut spans: Vec<Option<Span>> = vec![None; p.len()];
    let mut i = 0usize;
    let mut prev = Span { start: 0, end: 0 };
    for e in events {
        while i < p.len() && p[i] == b' ' {
            i += 1;
        }
        if i >= p.len() {
            break; // trace exhausted; the rest inherit below
        }
        let n = e.name.len();
        if i + n > p.len() || &p[i..i + n] != e.name.as_slice() {
            return None; // fail closed
        }
        // Clamp monotonic: a position that goes backwards would put a later mark before an
        // earlier one, and SAPI requires event offsets to be non-decreasing.
        let mut sp = span_at(e.text_position);
        if sp.start < prev.start {
            sp = prev;
        }
        for s in spans.iter_mut().take(i + n).skip(i) {
            *s = Some(sp);
        }
        prev = sp;
        i += n;
    }

    // Spaces and any tail the events didn't reach inherit their left neighbour; anything
    // before the first attributed byte inherits the first span.
    let first = spans.iter().flatten().next().copied()?;
    let mut last = first;
    for s in spans.iter_mut() {
        match s {
            Some(v) => last = *v,
            None => *s = Some(last),
        }
    }
    Some(spans.into_iter().map(|s| s.unwrap_or(first)).collect())
}
