// What a synthesis client needs, whatever transport it arrived on.
//
// Two clients reach the synth — Kindle's SAPI engine over the named pipe (pipe.rs) and the
// browser extension over loopback HTTP (webserve.rs) — and exactly one of them also drives
// Kindle's UI. `CoreCtx` is the half they share; the Kindle half lives in `pipe::KindleCtx`
// beside `KindleCtl`, and the browser half in `webserve::WebCtx` beside the endpoint and the
// OCR worker. Neither of those can reach the other's, which is the point: the browser path
// used to hold the whole pipe context, so `webserve.rs` depended on UI Automation to serve a
// page image.
//
// It is constructed ONCE in main and cloned per connection. The clone is cheap and, more
// importantly, shares rather than copies: one serialized synth worker (espeak has global
// state and the ORT session belongs to that worker), one `HostState` cell, one bench slot.
// Two of any of those is the bug this shape exists to make hard to write.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::native_synth::NativeSynth;
use crate::state::HostState;

/// The synth-side context: where settings and voices live, the one worker, and the general
/// host state. No Kindle types, no transport types.
#[derive(Clone)]
pub struct CoreCtx {
    /// The app-data dir — `controls.json` lives here, re-read live per utterance/sub-frame
    /// rather than cached, so a slider move lands on the next chunk.
    pub app_data: PathBuf,
    /// The model dir (`<app_data>/<MODEL_ID>`), used to enumerate the narrators actually
    /// downloaded. Kept beside `app_data` rather than re-derived here so `MODEL_ID` stays
    /// owned by one place (main.rs).
    pub model_base: PathBuf,
    /// The serialized native synth worker. Cloning shares the one worker.
    pub native: NativeSynth,
    /// General host state: the "audio just went out" clock every client stamps, and the
    /// single bench slot. What the host believes *Kindle* is doing is not here — that is
    /// `kindle_state::KindleState`, reached only from the Kindle context.
    pub state: Arc<HostState>,
}

/// Narrators actually present on disk (`<model_base>/voices/<id>.bin`), sorted. Enumerated
/// rather than read from model-manifest.json so the list is what can really be synthesized
/// right now — a half-downloaded model advertises only what it has, and a client's picker
/// never offers a voice whose .bin is missing.
///
/// Shared by both transports rather than reimplemented per client: a second copy could only
/// drift, and the two would then disagree about which narrators exist.
pub fn available_voices(model_base: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(model_base.join("voices"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_suffix(".bin")
                .map(str::to_string)
        })
        .collect();
    v.sort();
    v
}
