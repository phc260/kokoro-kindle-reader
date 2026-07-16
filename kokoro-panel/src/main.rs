// Native settings panel for Kokoro Kindle Reader (Slint / Fluent theme). Spawned on
// demand by the headless host's tray "Settings" item. Reads/writes the same
// controls.json the host reads per utterance/sub-frame, so a narrator/speed/gain/
// chunk change lands on Kindle's next page. Model download/verify, the Kindle-voice
// toggle, and Preview (synth via the host pipe = WYSIWYG) are all here.
//
// The UI is declared in ui/panel.slint (compiled by build.rs); this file wires its
// properties/callbacks to the framework-agnostic logic in download.rs / preview.rs.
// Background work (download, verify, preview) runs on threads and pushes results back
// via `upgrade_in_event_loop`. The Kindle-voice toggle just persists a flag (no thread).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod download;
mod kindle_reader;
mod preview;

slint::include_modules!();

// Same identifier as the host: controls.json lives under %APPDATA%\<identifier>.
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
// Embedded so the narrator list stays in sync with what actually downloads.
const MANIFEST_JSON: &str = include_str!("../../model-manifest.json");
const DEFAULT_VOICE: &str = "af_heart";

fn app_data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(APP_IDENTIFIER)
}

fn controls_path() -> PathBuf {
    app_data_dir().join("controls.json")
}

/// A narrator, derived from a manifest voice entry (voices/<id>.bin).
struct Voice {
    id: String,
    name: String,  // "Heart"
    group: String, // "American — Female"
}

/// Pretty display name from an id: "af_heart" -> "Heart".
fn pretty_name(id: &str) -> String {
    let suffix = id.split_once('_').map(|(_, s)| s).unwrap_or(id);
    let mut chars = suffix.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

/// Group label from the id prefix: first char = language (a=American, b=British),
/// second = gender (f=Female, m=Male).
fn group_of(id: &str) -> String {
    let b = id.as_bytes();
    let lang = match b.first() {
        Some(b'a') => "American",
        Some(b'b') => "British",
        _ => "Other",
    };
    let gender = match b.get(1) {
        Some(b'f') => "Female",
        Some(b'm') => "Male",
        _ => "",
    };
    if gender.is_empty() {
        lang.to_string()
    } else {
        format!("{lang} — {gender}")
    }
}

fn load_voices() -> Vec<Voice> {
    let mut out = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(MANIFEST_JSON) {
        if let Some(files) = v.get("files").and_then(|f| f.as_array()) {
            for f in files {
                let path = f.get("path").and_then(|p| p.as_str()).unwrap_or("");
                if let Some(id) = path.strip_prefix("voices/").and_then(|s| s.strip_suffix(".bin")) {
                    out.push(Voice {
                        id: id.to_string(),
                        name: pretty_name(id),
                        group: group_of(id),
                    });
                }
            }
        }
    }
    out
}

/// A short self-introduction spoken as the preview sample (mirrors voiceIntro in
/// src/voices.ts).
fn intro_for(voice: &str, voices: &[Voice]) -> String {
    match voices.iter().find(|v| v.id == voice) {
        Some(v) => {
            let accent = if v.group.starts_with("American") {
                "American"
            } else if v.group.starts_with("British") {
                "British"
            } else {
                "Kokoro"
            };
            format!(
                "Hi, I'm {}, your {} narrator. I'd be glad to read your text aloud.",
                v.name, accent
            )
        }
        None => "Hi, I'd be glad to read your text aloud.".to_string(),
    }
}

/// Pre-synthesized narrator intro, so Preview plays instantly. Holds the voice id
/// the samples were rendered for (playback validates it) or `None` when empty/stale.
type PreviewCache = Arc<Mutex<Option<(String, Vec<f32>)>>>;

/// Kick a background synth of `voice`'s intro line into `cache`. A generation
/// counter (`gen`) makes the latest request win: an earlier, slower synth that
/// finishes after a newer one started is discarded, so rapid narrator changes
/// never cache a stale voice. Failure (e.g. host down) leaves the cache untouched.
fn prefetch_intro(voice: &str, voices: &[Voice], cache: &PreviewCache, gen: &Arc<AtomicU64>) {
    let text = intro_for(voice, voices);
    let voice = voice.to_string();
    let cache = cache.clone();
    let gen = gen.clone();
    let my_gen = gen.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        if let Ok(samples) = preview::synth(&text) {
            if gen.load(Ordering::SeqCst) == my_gen {
                *cache.lock().unwrap() = Some((voice, samples));
            }
        }
    });
}

/// Prefetch the intro for the UI's currently-selected narrator, but only once the
/// engine is ready (otherwise a synth would just fail against an absent model).
fn prefetch_for_current(ui: &AppWindow, voices: &[Voice], cache: &PreviewCache, gen: &Arc<AtomicU64>) {
    if ui.get_model_ready() {
        if let Some(v) = current_voice_id(ui, voices) {
            prefetch_intro(&v, voices, cache, gen);
        }
    }
}

// --- narrator cascading-dropdown helpers ------------------------------------

/// A Slint string model from &str slices (for the accent/gender lists).
fn str_model(items: &[&str]) -> slint::ModelRc<slint::SharedString> {
    let v: Vec<slint::SharedString> = items.iter().map(|s| (*s).into()).collect();
    slint::ModelRc::new(slint::VecModel::from(v))
}

/// Accent index from an id's first char: American (a) = 0, British (b) = 1.
fn accent_idx(id: &str) -> i32 {
    if id.as_bytes().first() == Some(&b'b') { 1 } else { 0 }
}

/// Gender index from an id's second char: Female (f) = 0, Male (m) = 1.
fn gender_idx(id: &str) -> i32 {
    if id.as_bytes().get(1) == Some(&b'm') { 1 } else { 0 }
}

/// Voices matching the currently-selected accent + gender, in manifest order.
fn filtered_voices<'a>(ui: &AppWindow, voices: &'a [Voice]) -> Vec<&'a Voice> {
    let a = ui.get_accent_index();
    let g = ui.get_gender_index();
    voices
        .iter()
        .filter(|v| accent_idx(&v.id) == a && gender_idx(&v.id) == g)
        .collect()
}

/// Rebuild the name dropdown for the current accent + gender. `keep` selects that
/// voice if it's in the new list, else the first entry.
fn refilter(ui: &AppWindow, voices: &[Voice], keep: Option<&str>) {
    let f = filtered_voices(ui, voices);
    let names: Vec<slint::SharedString> = f.iter().map(|v| v.name.clone().into()).collect();
    ui.set_names(slint::ModelRc::new(slint::VecModel::from(names)));
    let ni = keep
        .and_then(|k| f.iter().position(|v| v.id == k))
        .unwrap_or(0) as i32;
    ui.set_name_index(ni);
}

/// The voice id currently selected by the three dropdowns (if any).
fn current_voice_id(ui: &AppWindow, voices: &[Voice]) -> Option<String> {
    let n = ui.get_name_index();
    filtered_voices(ui, voices)
        .get(n as usize)
        .map(|v| v.id.clone())
}

/// Persist the currently-selected voice to controls.json.
fn commit_voice(ui: &AppWindow, voices: &[Voice], controls: &Arc<Mutex<Controls>>) {
    if let Some(id) = current_voice_id(ui, voices) {
        let mut c = controls.lock().unwrap();
        c.voice = id;
        c.save();
    }
}

/// The persisted settings (controls.json).
struct Controls {
    voice: String,
    speed: f32,
    gain: f32,
    chunk: u32,
    kindle_kokoro: bool,
    // Live pause command, not a persisted setting: while true the host stalls the
    // audio stream mid-page. Kept in the struct (and save()) so an unrelated save
    // (e.g. a volume change) doesn't drop the key and silently un-pause the host.
    paused: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            voice: DEFAULT_VOICE.to_string(),
            speed: 1.0,
            gain: 1.0,
            chunk: 2,
            kindle_kokoro: true,
            paused: false,
        }
    }
}

impl Controls {
    fn load() -> Controls {
        let mut c = Controls::default();
        if let Ok(txt) = std::fs::read_to_string(controls_path()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                if let Some(x) = v.get("voice").and_then(|x| x.as_str()) {
                    c.voice = x.to_string();
                }
                if let Some(x) = v.get("speed").and_then(|x| x.as_f64()) {
                    c.speed = x as f32;
                }
                if let Some(x) = v.get("gain").and_then(|x| x.as_f64()) {
                    c.gain = x as f32;
                }
                if let Some(x) = v.get("chunk").and_then(|x| x.as_u64()) {
                    c.chunk = x as u32;
                }
                if let Some(x) = v.get("kindle_kokoro").and_then(|x| x.as_bool()) {
                    c.kindle_kokoro = x;
                }
                if let Some(x) = v.get("paused").and_then(|x| x.as_bool()) {
                    c.paused = x;
                }
            }
        }
        c
    }

    fn save(&self) {
        let dir = app_data_dir();
        let _ = std::fs::create_dir_all(&dir);
        let json = serde_json::json!({
            "voice": self.voice,
            "speed": self.speed,
            "gain": self.gain,
            "chunk": self.chunk,
            "kindle_kokoro": self.kindle_kokoro,
            "paused": self.paused,
        });
        let txt = serde_json::to_string_pretty(&json).unwrap_or_default();
        let _ = std::fs::write(dir.join("controls.json"), txt);
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let app_data = app_data_dir();
    let voices = Arc::new(load_voices());
    let controls = Arc::new(Mutex::new(Controls::load()));

    let ui = AppWindow::new()?;

    // Narrator: three cascading dropdowns (accent x gender -> name), seeded from the
    // saved voice.
    ui.set_accents(str_model(&["American", "British"]));
    ui.set_genders(str_model(&["Female", "Male"]));
    let cur_voice = controls.lock().unwrap().voice.clone();
    ui.set_accent_index(accent_idx(&cur_voice));
    ui.set_gender_index(gender_idx(&cur_voice));
    refilter(&ui, &voices, Some(&cur_voice));
    {
        let c = controls.lock().unwrap();
        // Snap to 5% so the initial readout is a multiple of 5 (matches the sliders).
        ui.set_speed((c.speed / 0.05).round() * 0.05);
        ui.set_gain((c.gain / 0.05).round() * 0.05);
        ui.set_chunk(c.chunk as f32);
        ui.set_kindle_kokoro(c.kindle_kokoro);
        ui.set_paused(c.paused);
    }
    ui.set_model_ready(download::model_complete(&app_data));

    // Shared background-task guards.
    let dl_running = Arc::new(AtomicBool::new(false));
    let dl_progress = Arc::new(Mutex::new(download::Progress::default()));
    let verify_running = Arc::new(AtomicBool::new(false));

    // Pre-synthesized narrator intro so Preview is instant. Populated when the
    // engine becomes ready and on every narrator change; invalidated when speed/
    // gain change (so the buffered clip never plays stale settings).
    let preview_cache: PreviewCache = Arc::new(Mutex::new(None));
    let preview_gen = Arc::new(AtomicU64::new(0));

    // --- controls callbacks (UI thread) ---
    // Narrator: accent/gender re-filter the name list (reset to its first entry);
    // any of the three commits the resulting voice to controls.json.
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        let cache = preview_cache.clone();
        let gen = preview_gen.clone();
        ui.on_accent_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                refilter(&ui, &voices, None);
                commit_voice(&ui, &voices, &controls);
                prefetch_for_current(&ui, &voices, &cache, &gen);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        let cache = preview_cache.clone();
        let gen = preview_gen.clone();
        ui.on_gender_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                refilter(&ui, &voices, None);
                commit_voice(&ui, &voices, &controls);
                prefetch_for_current(&ui, &voices, &cache, &gen);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        let cache = preview_cache.clone();
        let gen = preview_gen.clone();
        ui.on_name_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                commit_voice(&ui, &voices, &controls);
                prefetch_for_current(&ui, &voices, &cache, &gen);
            }
        });
    }
    {
        let controls = controls.clone();
        let cache = preview_cache.clone();
        ui.on_speed_changed(move |v| {
            {
                let mut c = controls.lock().unwrap();
                c.speed = v;
                c.save();
            }
            // Speed is baked into the synthesized samples — drop the stale buffer.
            *cache.lock().unwrap() = None;
        });
    }
    {
        let controls = controls.clone();
        let cache = preview_cache.clone();
        ui.on_gain_changed(move |v| {
            {
                let mut c = controls.lock().unwrap();
                c.gain = v;
                c.save();
            }
            // Gain is baked into the synthesized samples — drop the stale buffer.
            *cache.lock().unwrap() = None;
        });
    }
    {
        let controls = controls.clone();
        ui.on_chunk_changed(move |v| {
            let mut c = controls.lock().unwrap();
            c.chunk = v.round().max(1.0) as u32;
            c.save();
        });
    }

    // --- download ---
    {
        let ui_weak = ui.as_weak();
        let app_data = app_data.clone();
        let dl_running = dl_running.clone();
        let dl_progress = dl_progress.clone();
        let voices = voices.clone();
        let preview_cache = preview_cache.clone();
        let preview_gen = preview_gen.clone();
        ui.on_download_clicked(move || {
            if dl_running.load(Ordering::SeqCst) {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_downloading(true);
                ui.set_status(slint::SharedString::new());
            }
            let progress = dl_progress.clone();
            let app_data_r = app_data.clone();
            let weak = ui_weak.clone();
            let voices = voices.clone();
            let cache = preview_cache.clone();
            let gen = preview_gen.clone();
            let repaint = move || {
                let p = progress.lock().unwrap().clone();
                let app_data = app_data_r.clone();
                let voices = voices.clone();
                let cache = cache.clone();
                let gen = gen.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let frac = if p.total > 0 {
                        p.downloaded as f32 / p.total as f32
                    } else {
                        0.0
                    };
                    ui.set_download_frac(frac);
                    ui.set_download_label(
                        format!(
                            "Downloading {} — {:.0} / {:.0} MB",
                            p.file,
                            p.downloaded as f32 / 1e6,
                            p.total as f32 / 1e6
                        )
                        .into(),
                    );
                    if p.done {
                        ui.set_downloading(false);
                        ui.set_model_ready(download::model_complete(&app_data));
                        ui.set_status(match p.error {
                            Some(e) => format!("Download error: {e}").into(),
                            None => "Model downloaded.".into(),
                        });
                        // Engine just became ready — warm the preview buffer.
                        prefetch_for_current(&ui, &voices, &cache, &gen);
                    }
                });
            };
            download::start(app_data.clone(), dl_running.clone(), dl_progress.clone(), repaint);
        });
    }

    // --- auto-verify at startup (no button) ---
    // If the model is present, hash it against the manifest in the background,
    // driving a determinate progress bar (verify-frac) as the data is checked, and
    // repair-flag any corrupt files. Success is silent (the card returns to "Model
    // ready"); only a repair surfaces a status line.
    //
    // Debug builds skip this hash-check: re-reading the whole multi-hundred-MB model on
    // every relaunch is wasted work in dev, and `model_complete` above already reported
    // the model ready. We still warm the preview buffer as the verify path would on
    // success. (`cfg!` keeps both arms compiling, so `verify_running` stays live.)
    if download::model_complete(&app_data) && cfg!(debug_assertions) {
        prefetch_for_current(&ui, &voices, &preview_cache, &preview_gen);
    } else if download::model_complete(&app_data) {
        verify_running.store(true, Ordering::SeqCst);
        ui.set_verifying(true);
        ui.set_verify_frac(0.0);
        let app_data_r = app_data.clone();
        let weak = ui.as_weak();
        let running = verify_running.clone();
        let voices_v = voices.clone();
        let cache_v = preview_cache.clone();
        let gen_v = preview_gen.clone();
        std::thread::spawn(move || {
            // Push a frame only when the whole-percent changes — hashing reports
            // every 64 KB, far more often than the UI needs to repaint.
            let mut last_pct: i32 = -1;
            let (checked, repaired) = download::verify(&app_data_r, |done, total| {
                let pct = if total > 0 { (done * 100 / total) as i32 } else { 100 };
                if pct != last_pct {
                    last_pct = pct;
                    let frac = done as f32 / total.max(1) as f32;
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_verify_frac(frac));
                }
            });
            running.store(false, Ordering::SeqCst);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_verifying(false);
                ui.set_model_ready(download::model_complete(&app_data_r));
                if repaired > 0 {
                    ui.set_status(
                        format!(
                            "{repaired} of {checked} model files were corrupt and removed — click Download to repair."
                        )
                        .into(),
                    );
                }
                // Engine is ready (model verified) — warm the preview buffer.
                prefetch_for_current(&ui, &voices_v, &cache_v, &gen_v);
            });
        });
    }

    // --- Kindle-voice toggle (persist only) ---
    // Just records `kindle_kokoro` in controls.json. The host's Kindle-watcher reads this
    // flag live and injects (or doesn't) the Kokoro hook on Kindle's next launch — no UAC,
    // no elevated guard, nothing to fail, so no busy state or revert.
    {
        let ui_weak = ui.as_weak();
        let controls = controls.clone();
        ui.on_kindle_toggled(move |desired| {
            {
                let mut c = controls.lock().unwrap();
                c.kindle_kokoro = desired;
                c.save();
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_status(
                    if desired {
                        "Kokoro will narrate Kindle. Reopen Kindle if it's already open."
                    } else {
                        "Kindle will use its own voice on next launch."
                    }
                    .into(),
                );
            }
        });
    }

    // --- Preview (buffered if pre-synthesized, else synth via the host pipe) ---
    // Shared handle to the playing sink so the Stop button can halt it mid-line.
    let active_sink = preview::new_active();
    {
        let ui_weak = ui.as_weak();
        let controls = controls.clone();
        let voices = voices.clone();
        let cache = preview_cache.clone();
        let active_sink = active_sink.clone();
        ui.on_preview_clicked(move || {
            let already = ui_weak.upgrade().map(|ui| ui.get_previewing()).unwrap_or(true);
            if already {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_previewing(true);
                ui.set_status(slint::SharedString::new());
            }
            let voice = controls.lock().unwrap().voice.clone();
            // Use the pre-synthesized buffer if it matches the current voice;
            // otherwise fall back to an on-demand synth of the intro.
            let buffered = match &*cache.lock().unwrap() {
                Some((v, s)) if *v == voice && !s.is_empty() => Some(s.clone()),
                _ => None,
            };
            let text = intro_for(&voice, &voices);
            let weak = ui_weak.clone();
            let active_sink = active_sink.clone();
            std::thread::spawn(move || {
                let res = match buffered {
                    Some(samples) => preview::play_samples(samples, &active_sink),
                    None => preview::play(&text, &active_sink),
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_previewing(false);
                    if let Err(e) = res {
                        ui.set_status(e.into());
                    }
                });
            });
        });
    }

    // --- Stop preview (halt the sink; the playing thread clears `previewing`) ---
    {
        let active_sink = active_sink.clone();
        ui.on_stop_clicked(move || {
            preview::stop(&active_sink);
        });
    }

    // Set while the panel is driving Kindle's toggle (and briefly after), so the
    // state poll below doesn't read a stale value mid-flip and fight the switch.
    let reader_busy = Arc::new(AtomicBool::new(false));

    // --- Read Aloud (drive Kindle's Assistive reader via UI Automation) ---
    // The switch already flipped `reading` optimistically; `want` is that new value.
    // Drive Kindle to match, then write back the real state (revert on failure).
    {
        let ui_weak = ui.as_weak();
        let controls = controls.clone();
        let reader_busy = reader_busy.clone();
        let active_sink = active_sink.clone();
        ui.on_read_aloud_clicked(move |want| {
            // Toggling the reader hands the transport over to Kindle, so silence any
            // preview still playing — its thread then clears `previewing`. Otherwise a
            // preview started before Read Aloud would keep playing over Kindle's
            // narration with its Stop button hidden by the reading-state view.
            preview::stop(&active_sink);
            // Starting or stopping reading always clears any pause, so a fresh
            // Read Aloud never begins stalled and a stop leaves no lingering pause.
            {
                let mut c = controls.lock().unwrap();
                c.paused = false;
                c.save();
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_paused(false);
                ui.set_status(slint::SharedString::new());
            }
            let weak = ui_weak.clone();
            let reader_busy = reader_busy.clone();
            reader_busy.store(true, Ordering::SeqCst);
            std::thread::spawn(move || {
                let res = kindle_reader::set_read_aloud(want);
                let _ = weak.upgrade_in_event_loop(move |ui| match res {
                    Ok(reading) => {
                        ui.set_reading(reading);
                        ui.set_status(
                            if reading { "Reading started in Kindle." } else { "Reading stopped." }
                                .into(),
                        );
                    }
                    Err(e) => {
                        ui.set_reading(!want); // revert the optimistic switch flip
                        ui.set_status(e.into());
                    }
                });
                // Kindle updates the toggle's UIA state asynchronously; hold the
                // poll off a moment longer so it doesn't read the pre-flip value
                // and bounce the switch back.
                std::thread::sleep(Duration::from_millis(100));
                reader_busy.store(false, Ordering::SeqCst);
            });
        });
    }

    // --- Pause / Resume (stall the host's audio stream via controls.json `paused`) ---
    // Just persists `paused`; the host reads it live per sub-frame and stalls the
    // stream mid-page (Kindle keeps the page). No UIA, no thread needed.
    {
        let controls = controls.clone();
        let weak = ui.as_weak();
        ui.on_pause_toggled(move |want| {
            {
                let mut c = controls.lock().unwrap();
                c.paused = want;
                c.save();
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_paused(want);
                ui.set_status(if want { "Paused." } else { "Resumed." }.into());
            }
        });
    }

    // --- Sync the Read Aloud switch when Assistive reader is toggled in Kindle ---
    // The panel only ever drives Kindle; nothing read Kindle's state back, so a
    // toggle made inside Kindle left the switch stale. Poll the toggle's state on a
    // timer and mirror it onto the switch. Best-effort: a `None` read (toolbar
    // hidden / Kindle closed) keeps the last known state; we skip while the panel is
    // itself driving the toggle (reader_busy) and never spawn overlapping polls.
    let poll_timer = slint::Timer::default();
    {
        let weak = ui.as_weak();
        let poll_busy = Arc::new(AtomicBool::new(false));
        let reader_busy = reader_busy.clone();
        poll_timer.start(slint::TimerMode::Repeated, Duration::from_millis(100), move || {
            if reader_busy.load(Ordering::SeqCst) {
                return;
            }
            if poll_busy.swap(true, Ordering::SeqCst) {
                return; // a previous poll is still running
            }
            let weak = weak.clone();
            let poll_busy = poll_busy.clone();
            let reader_busy = reader_busy.clone();
            std::thread::spawn(move || {
                let state = kindle_reader::read_state();
                // Is the host streaming audio right now? Drives the live "Speaking"
                // indicator. None (host unreachable) reads as not speaking. But the panel's
                // own intro prefetch also streams from the host (playing nothing), stamping
                // the same clock — so discount a host reading we just caused ourselves.
                let host_speaking = preview::host_speaking().unwrap_or(false);
                let self_synth = preview::self_synth_recent();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    // Speaking = an audible Preview (tracked by `previewing`) OR the host
                    // streaming for Kindle (host reading that wasn't our own prefetch).
                    let speaking = ui.get_previewing() || (host_speaking && !self_synth);
                    if ui.get_speaking() != speaking {
                        ui.set_speaking(speaking);
                    }
                    // Re-check reader_busy: a user flip may have started while we
                    // were reading. Only apply a definite state that differs.
                    if !reader_busy.load(Ordering::SeqCst) {
                        if let Some(on) = state {
                            if ui.get_reading() != on {
                                ui.set_reading(on);
                            }
                        }
                    }
                });
                poll_busy.store(false, Ordering::SeqCst);
            });
        });
    }

    ui.run()
}
