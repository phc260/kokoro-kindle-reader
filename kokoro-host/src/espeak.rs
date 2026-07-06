// espeak-ng FFI + one-segment phoneme trace (the phonemizer path kokoro-js uses via
// espeak). espeak keeps global state and isn't thread-safe — single worker only. The
// phoneme trace goes to a FILE*, so we FFI the CRT's fopen/fclose to feed it one.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum FILE {}

// espeak constants (from speak_lib.h / espeak_ng.h).
const ENOUTPUT_MODE_SYNCHRONOUS: c_int = 0x0001;
const POS_CHARACTER: c_int = 1;
const ESPEAK_CHARS_UTF8: c_uint = 1;
const ESPEAK_PHONEMES: c_uint = 0x100;
const ENS_OK: c_int = 0;
const EE_OK: c_int = 0;

type SynthCallback = extern "C" fn(*mut i16, c_int, *mut c_void) -> c_int;

extern "C" {
    fn espeak_ng_InitializePath(path: *const c_char);
    fn espeak_ng_Initialize(context: *mut *mut c_void) -> c_int;
    fn espeak_ng_InitializeOutput(output_mode: c_int, buffer_length: c_int, device: *const c_char) -> c_int;
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

extern "C" fn discard_audio(_wav: *mut i16, _n: c_int, _events: *mut c_void) -> c_int {
    0 // we only want the phoneme trace, not audio
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
        espeak_SetSynthCallback(discard_audio);
        let voice = CString::new("en-us").unwrap();
        if espeak_SetVoiceByName(voice.as_ptr()) != EE_OK {
            return Err("espeak_SetVoiceByName(en-us) failed".into());
        }
    }
    Ok(())
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// espeak synth-trace phonemization of one punctuation-free UTF-8 segment.
/// Mirrors PhonemizeSegment: trace to a temp FILE, then fold clause-per-line into
/// a single space-joined string.
pub fn phonemize_segment(text: &[u8]) -> Vec<u8> {
    // unique temp path
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("kokoro_phon_{pid}_{n}.txt"));
    let tmp_c = match CString::new(tmp.to_string_lossy().to_string()) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mode = CString::new("wb+").unwrap();

    // text must be NUL-terminated; size includes the terminator.
    let text_c = match CString::new(text) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let bytes_with_nul = text_c.as_bytes_with_nul();

    unsafe {
        let f = fopen(tmp_c.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Vec::new();
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
    p
}
