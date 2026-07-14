//! The SAPI5 voice engine object + its class factory. `Speak` forwards the whole
//! utterance to kokoro-host over the pipe and streams the returned sub-frames straight
//! to the SAPI site in ~250 ms blocks (no buffering) while reporting SAPI events
//! (word/sentence boundaries + `<bookmark>`s) as it goes. The event reporting is what
//! lets Kindle 18632's event-driven narrator advance and highlight past the first
//! sentence.

use core::ffi::c_void;
use core::ptr::null_mut;
use std::sync::Mutex;

use windows::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_FAIL, E_OUTOFMEMORY, E_POINTER, LPARAM, S_OK, WPARAM,
};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::{
    SPEI_SENTENCE_BOUNDARY, SPEI_TTS_BOOKMARK, SPEI_WORD_BOUNDARY, SPET_LPARAM_IS_STRING,
    SPET_LPARAM_IS_UNDEFINED, SPEVENT, SPVA_Bookmark, SPVA_Pronounce, SPVA_Speak, SPVA_SpellOut,
    SPVTEXTFRAG, SPVES_ABORT, SPVES_VOLUME,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Impl};
use windows_core::{implement, IUnknown, Interface, Ref, BOOL, GUID, HRESULT};

const WAVE_FORMAT_PCM: u16 = 1;

use crate::sapi::{
    ISpObjectWithToken, ISpObjectWithToken_Impl, ISpTTSEngine, ISpTTSEngineSite, ISpTTSEngine_Impl,
    SPDFID_WAVEFORMATEX,
};
use crate::worker::Frame;
use crate::{SYNTH_LOCK, WORKER};

const SAMPLE_RATE: u32 = 24000;

/// A SAPI event to report to the site as audio is produced. Kindle 1.0.18632's
/// narrator (`SpVoiceEngine`/`NarratorService` in xrm120.dll) drives sentence-to-
/// sentence advancement and karaoke highlighting from these — its
/// `WordBoundaryListHandler` collects word boundaries and it matches each `<bookmark>`
/// to a word position. Without them it speaks the first unit and never advances (the
/// "only the first sentence of each page is synthesized" bug). We don't have per-word
/// timing from Kokoro, so each event is placed by its character position in the
/// concatenated speak text, mapped to an audio-stream byte offset once the utterance's
/// total audio length is known. `concat` is that character position.
enum SpeakEvent {
    /// SPEI_WORD_BOUNDARY: `src_pos`/`src_len` are the word's position + length in the
    /// original text SAPI handed us (so the host can map it back to its SSML).
    Word { src_pos: u32, src_len: u32, concat: usize },
    /// SPEI_SENTENCE_BOUNDARY at the start of a speakable fragment.
    Sentence { src_pos: u32, concat: usize },
    /// SPEI_TTS_BOOKMARK: `mark` is NUL-terminated (SAPI copies it), `value` its
    /// leading integer (the classic `wParam` bookmark id).
    Bookmark { mark: Vec<u16>, value: isize, concat: usize },
}

impl SpeakEvent {
    fn concat(&self) -> usize {
        match self {
            SpeakEvent::Word { concat, .. }
            | SpeakEvent::Sentence { concat, .. }
            | SpeakEvent::Bookmark { concat, .. } => *concat,
        }
    }
}

fn is_ws16(c: u16) -> bool {
    c == 0x20 || c == 0x09 || c == 0x0D || c == 0x0A
}

/// C-`wcstol`-ish: leading integer of a UTF-16 string (bookmark ids are numeric).
fn parse_leading_int16(s: &[u16]) -> isize {
    let mut i = 0;
    while i < s.len() && (s[i] == 0x20 || s[i] == 0x09) {
        i += 1;
    }
    let mut sign = 1isize;
    if i < s.len() && (s[i] == 0x2D || s[i] == 0x2B) {
        if s[i] == 0x2D {
            sign = -1;
        }
        i += 1;
    }
    let mut n = 0isize;
    while i < s.len() && (0x30..=0x39).contains(&s[i]) {
        n = n * 10 + (s[i] - 0x30) as isize;
        i += 1;
    }
    sign * n
}

/// Report one SAPI event to the site at `off` bytes into the audio stream, but only if
/// the host registered interest in that event id (`GetEventInterest`). `ptype` is the
/// SPEVENTLPARAMTYPE; for a bookmark string SAPI copies `lparam` during the call.
unsafe fn add_sapi_event(
    site: &ISpTTSEngineSite,
    interest: u64,
    id: i32,
    ptype: i32,
    wparam: usize,
    lparam: isize,
    off: u64,
) {
    if !(0..64).contains(&id) || (interest >> id) & 1 == 0 {
        return;
    }
    let mut e: SPEVENT = core::mem::zeroed();
    // eEventId (low 16) | elParamType (high 16), per the SPEVENT bitfield.
    e._bitfield = (id & 0xFFFF) | ((ptype & 0xFFFF) << 16);
    e.ullAudioStreamOffset = off;
    e.wParam = WPARAM(wparam);
    e.lParam = LPARAM(lparam);
    let _ = site.AddEvents(&e as *const SPEVENT as *const c_void, 1);
}

/// Fire every pending event that belongs to the current chunk and whose mapped audio
/// offset has been reached (`<= limit` bytes written). Each event's `concat` (a UTF-16
/// index into the request text) maps linearly onto the chunk's audio: offset =
/// `chunk_base + (concat - chunk_start) / chunk_u16 * chunk_samples * 2`. Events for a
/// later chunk (concat past this chunk) wait for their own `Frame::Chunk`. Offsets are
/// clamped non-decreasing (`last_off`) as SAPI expects.
#[allow(clippy::too_many_arguments)]
unsafe fn emit_ready_events(
    site: &ISpTTSEngineSite,
    interest: u64,
    events: &[SpeakEvent],
    ev: &mut usize,
    last_off: &mut u64,
    chunk_start: usize,
    chunk_u16: usize,
    chunk_samples: u64,
    chunk_base: u64,
    limit: u64,
) {
    while *ev < events.len() {
        let c = events[*ev].concat();
        if c >= chunk_start + chunk_u16 {
            break; // belongs to a later chunk; wait for its Frame::Chunk
        }
        let local = c.saturating_sub(chunk_start) as u64; // clamp any drift straggler
        let mapped = if chunk_u16 > 0 {
            chunk_base + local * chunk_samples * 2 / chunk_u16 as u64
        } else {
            chunk_base
        } & !1;
        if mapped > limit {
            break; // not reached in the audio written so far
        }
        let off = mapped.max(*last_off);
        report_event(site, interest, &events[*ev], off);
        *last_off = off;
        *ev += 1;
    }
}

/// Report a collected `SpeakEvent` to the site at audio-stream byte offset `off`.
unsafe fn report_event(site: &ISpTTSEngineSite, interest: u64, ev: &SpeakEvent, off: u64) {
    match ev {
        SpeakEvent::Word { src_pos, src_len, .. } => add_sapi_event(
            site,
            interest,
            SPEI_WORD_BOUNDARY.0,
            SPET_LPARAM_IS_UNDEFINED.0,
            *src_len as usize,
            *src_pos as isize,
            off,
        ),
        SpeakEvent::Sentence { src_pos, .. } => add_sapi_event(
            site,
            interest,
            SPEI_SENTENCE_BOUNDARY.0,
            SPET_LPARAM_IS_UNDEFINED.0,
            0,
            *src_pos as isize,
            off,
        ),
        SpeakEvent::Bookmark { mark, value, .. } => add_sapi_event(
            site,
            interest,
            SPEI_TTS_BOOKMARK.0,
            SPET_LPARAM_IS_STRING.0,
            *value as usize,
            mark.as_ptr() as isize,
            off,
        ),
    }
}

#[implement(ISpTTSEngine, ISpObjectWithToken)]
pub struct KokoroEngine {
    // The voice token SAPI hands us (held for its lifetime; never read).
    token: Mutex<Option<IUnknown>>,
}

impl KokoroEngine {
    pub fn new() -> Self {
        KokoroEngine { token: Mutex::new(None) }
    }
}

impl ISpObjectWithToken_Impl for KokoroEngine_Impl {
    unsafe fn SetObjectToken(&self, token: *mut c_void) -> HRESULT {
        let held = IUnknown::from_raw_borrowed(&token).cloned(); // clone = AddRef
        *self.token.lock().unwrap() = held;
        S_OK
    }

    unsafe fn GetObjectToken(&self, out: *mut *mut c_void) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        match self.token.lock().unwrap().as_ref() {
            Some(tok) => {
                *out = tok.clone().into_raw(); // AddRef, transfer to caller
                S_OK
            }
            None => E_FAIL,
        }
    }
}

impl ISpTTSEngine_Impl for KokoroEngine_Impl {
    // Declare 24 kHz / 16-bit / mono PCM (Kokoro's native rate); SAPI inserts any
    // converter a host needs.
    unsafe fn GetOutputFormat(
        &self,
        _target_fmt_id: *const GUID,
        _target_wfx: *const WAVEFORMATEX,
        out_fmt_id: *mut GUID,
        out_wfx: *mut *mut WAVEFORMATEX,
    ) -> HRESULT {
        if out_fmt_id.is_null() || out_wfx.is_null() {
            return E_POINTER;
        }
        let wfx = CoTaskMemAlloc(core::mem::size_of::<WAVEFORMATEX>()) as *mut WAVEFORMATEX;
        if wfx.is_null() {
            return E_OUTOFMEMORY;
        }
        *wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            wBitsPerSample: 16,
            nBlockAlign: 2,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            cbSize: 0,
        };
        *out_fmt_id = SPDFID_WAVEFORMATEX;
        *out_wfx = wfx;
        S_OK
    }

    unsafe fn Speak(
        &self,
        _flags: u32,
        _fmt_id: *const GUID,
        _wfx: *const WAVEFORMATEX,
        frags: *const SPVTEXTFRAG,
        site: *mut c_void,
    ) -> HRESULT {
        let Some(site) = ISpTTSEngineSite::from_raw_borrowed(&site) else {
            return E_POINTER;
        };

        // Connect to the host's synth pipe (silently skip if it isn't running).
        {
            let _lk = SYNTH_LOCK.lock().unwrap();
            if !WORKER.ensure_connected() {
                return E_FAIL;
            }
        }

        // Which events the host wants reported (Kindle asks for word/bookmark). We only
        // AddEvents for interested ids.
        let mut interest: u64 = 0;
        let _ = site.GetEventInterest(&mut interest);

        // Concatenate the speakable fragments (UTF-16 -> UTF-8) and, in the same pass,
        // collect the SAPI events to report: a word boundary per word, a sentence
        // boundary per speakable fragment, and each `<bookmark>` SAPI parsed out. Each
        // is tagged with its character position in the concatenated text (`concat`) so
        // we can place it in the audio stream once we know the total audio length.
        let mut utf16: Vec<u16> = Vec::new();
        let mut events: Vec<SpeakEvent> = Vec::new();
        let mut f = frags;
        while !f.is_null() {
            let frag = &*f;
            let a = frag.State.eAction;
            let src = frag.ulTextSrcOffset;
            if a == SPVA_Bookmark && !frag.pTextStart.is_null() && frag.ulTextLen > 0 {
                // The bookmark name (SAPI copies it when we report SPET_LPARAM_IS_STRING).
                let name = core::slice::from_raw_parts(frag.pTextStart.0, frag.ulTextLen as usize);
                let value = parse_leading_int16(name);
                let mut mark = name.to_vec();
                mark.push(0); // NUL-terminate for SPET_LPARAM_IS_STRING
                events.push(SpeakEvent::Bookmark { mark, value, concat: utf16.len() });
            } else if (a == SPVA_Speak || a == SPVA_Pronounce || a == SPVA_SpellOut)
                && !frag.pTextStart.is_null()
                && frag.ulTextLen > 0
            {
                let s = core::slice::from_raw_parts(frag.pTextStart.0, frag.ulTextLen as usize);
                let base = utf16.len();
                events.push(SpeakEvent::Sentence { src_pos: src, concat: base });
                // One word boundary per whitespace-delimited run.
                let mut i = 0usize;
                while i < s.len() {
                    while i < s.len() && is_ws16(s[i]) {
                        i += 1;
                    }
                    let w0 = i;
                    while i < s.len() && !is_ws16(s[i]) {
                        i += 1;
                    }
                    if i > w0 {
                        events.push(SpeakEvent::Word {
                            src_pos: src + w0 as u32,
                            src_len: (i - w0) as u32,
                            concat: base + w0,
                        });
                    }
                }
                utf16.extend_from_slice(s);
                utf16.push(0x20);
            }
            f = frag.pNext;
        }
        if utf16.is_empty() {
            return S_OK;
        }
        let text = String::from_utf16_lossy(&utf16);

        // Host SAPI rate -10..10 -> speed 1/3x..3x (log). The host folds in the user's
        // own speed. Rate is fixed for the utterance.
        let mut volume: u16 = 100;
        let _ = site.GetVolume(&mut volume);
        let mut rate: i32 = 0;
        let _ = site.GetRate(&mut rate);
        let speed = 3.0f32.powf(rate as f32 / 10.0);

        // Open the stream (one 'S' request), with a single reconnect retry.
        {
            let _lk = SYNTH_LOCK.lock().unwrap();
            if !WORKER.begin_synth(text.as_bytes(), speed)
                && !(WORKER.ensure_connected() && WORKER.begin_synth(text.as_bytes(), speed))
            {
                return E_FAIL;
            }
        }

        const BLOCK: usize = (SAMPLE_RATE / 4) as usize; // ~250 ms
        let mut result = S_OK;
        let mut aborted = false;

        // Stream the host's sub-frames straight through in ~250 ms blocks (no buffering,
        // so audio starts immediately and never gaps — Kindle sends the whole page in one
        // Speak and drops the stream if it goes quiet). Each `Frame::Chunk` header tells us
        // that chunk's UTF-16 span + sample count; as we write its audio we fire the
        // word/bookmark events that fall in it, at their true audio offsets, so Kindle's
        // per-word bookmark narrator advances and highlights in sync.
        let mut ev = 0usize;
        let mut last_off = 0u64;
        let mut bytes_written = 0u64;
        let mut chunk_start = 0usize; // UTF-16 index at the current chunk's start
        let mut chunk_u16 = 0usize; // current chunk's UTF-16 length
        let mut chunk_samples = 0u64; // current chunk's total samples
        let mut chunk_base = 0u64; // bytes written before the current chunk
        'stream: loop {
            if site.GetActions() & SPVES_ABORT.0 as u32 != 0 {
                aborted = true;
                break;
            }
            let frame = {
                let _lk = SYNTH_LOCK.lock().unwrap();
                WORKER.read_frame()
            };
            match frame {
                Frame::End => break,
                Frame::Error => {
                    // The host failed a chunk (e.g. a transient GPU error it couldn't
                    // retry past). If we've already streamed audio, end gracefully so
                    // Kindle plays it and advances to the next page rather than purging
                    // the queue and halting; only report failure if nothing came through.
                    if bytes_written == 0 {
                        result = E_FAIL;
                    }
                    break;
                }
                Frame::Chunk { u16_len, samples } => {
                    chunk_start += chunk_u16; // advance past the previous chunk
                    chunk_u16 = u16_len as usize;
                    chunk_samples = samples as u64;
                    chunk_base = bytes_written;
                }
                Frame::Data { samples: pcm, gain } => {
                    // f32 [-1,1] -> i16 with the frame's gain x the host volume.
                    if site.GetActions() & SPVES_VOLUME.0 as u32 != 0 {
                        let _ = site.GetVolume(&mut volume);
                    }
                    let vol = volume as f32 / 100.0;
                    let out: Vec<i16> = pcm
                        .iter()
                        .map(|&s| ((s * gain * vol).clamp(-1.0, 1.0) * 32767.0) as i16)
                        .collect();
                    for block in out.chunks(BLOCK) {
                        if site.GetActions() & SPVES_ABORT.0 as u32 != 0 {
                            aborted = true;
                            break 'stream;
                        }
                        let block_end = bytes_written + (block.len() * 2) as u64;
                        emit_ready_events(
                            site, interest, &events, &mut ev, &mut last_off, chunk_start,
                            chunk_u16, chunk_samples, chunk_base, block_end,
                        );
                        let mut wrote = 0u32;
                        let hr = site.Write(
                            block.as_ptr() as *const c_void,
                            (block.len() * 2) as u32,
                            &mut wrote,
                        );
                        if hr.is_err() {
                            result = hr;
                            aborted = true;
                            break 'stream;
                        }
                        bytes_written = block_end;
                    }
                }
            }
        }

        // Flush any events not yet fired (final chunk's tail / stragglers) at the end.
        if !aborted {
            while ev < events.len() {
                let off = bytes_written.max(last_off);
                report_event(site, interest, &events[ev], off);
                last_off = off;
                ev += 1;
            }
        }

        // Stopped early: close the pipe to interrupt the host's stream. Next Speak
        // reconnects. A clean End/Error leaves the pipe open for reuse.
        if aborted {
            WORKER.close();
        }
        result
    }
}

// ---- class factory --------------------------------------------------------

#[implement(IClassFactory)]
pub struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        if object.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *object = null_mut();
        }
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let engine: ISpTTSEngine = KokoroEngine::new().into();
        unsafe { engine.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}
