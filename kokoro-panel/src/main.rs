// Native settings panel for Kokoro Kindle Reader (Slint / Fluent theme). Spawned on
// demand by the headless host's tray "Settings" item. Reads/writes the same
// controls.json the host reads per utterance/sub-frame, so a narrator/speed/gain/
// chunk change lands on Kindle's next page. Model download/verify, the Kindle-voice
// toggle, and Preview (synth via the host pipe = WYSIWYG) are all here.
//
// The UI is declared in ui/panel.slint (compiled by build.rs); this file wires its
// properties/callbacks to the framework-agnostic logic in download.rs / kindle.rs /
// preview.rs. Background work (download, verify, elevated Kindle switch, preview)
// runs on threads and pushes results back via `upgrade_in_event_loop`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

mod download;
mod kindle;
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
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            voice: DEFAULT_VOICE.to_string(),
            speed: 1.0,
            gain: 1.0,
            chunk: 2,
            kindle_kokoro: true,
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
    }
    ui.set_model_ready(download::model_complete(&app_data));

    // Shared background-task guards.
    let dl_running = Arc::new(AtomicBool::new(false));
    let dl_progress = Arc::new(Mutex::new(download::Progress::default()));
    let verify_running = Arc::new(AtomicBool::new(false));
    let kindle_running = Arc::new(AtomicBool::new(false));

    // --- controls callbacks (UI thread) ---
    // Narrator: accent/gender re-filter the name list (reset to its first entry);
    // any of the three commits the resulting voice to controls.json.
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        ui.on_accent_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                refilter(&ui, &voices, None);
                commit_voice(&ui, &voices, &controls);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        ui.on_gender_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                refilter(&ui, &voices, None);
                commit_voice(&ui, &voices, &controls);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        ui.on_name_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                commit_voice(&ui, &voices, &controls);
            }
        });
    }
    {
        let controls = controls.clone();
        ui.on_speed_changed(move |v| {
            let mut c = controls.lock().unwrap();
            c.speed = v;
            c.save();
        });
    }
    {
        let controls = controls.clone();
        ui.on_gain_changed(move |v| {
            let mut c = controls.lock().unwrap();
            c.gain = v;
            c.save();
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
            let repaint = move || {
                let p = progress.lock().unwrap().clone();
                let app_data = app_data_r.clone();
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
    if download::model_complete(&app_data) {
        verify_running.store(true, Ordering::SeqCst);
        ui.set_verifying(true);
        ui.set_verify_frac(0.0);
        let app_data_r = app_data.clone();
        let weak = ui.as_weak();
        let running = verify_running.clone();
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
            });
        });
    }

    // --- Kindle-voice toggle (elevated) ---
    {
        let ui_weak = ui.as_weak();
        let kindle_running = kindle_running.clone();
        let controls = controls.clone();
        ui.on_kindle_toggled(move |desired| {
            if kindle_running.swap(true, Ordering::SeqCst) {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_kindle_busy(true);
            }
            let weak = ui_weak.clone();
            let running = kindle_running.clone();
            let controls = controls.clone();
            std::thread::spawn(move || {
                let res = kindle::set_voice(desired);
                running.store(false, Ordering::SeqCst);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_kindle_busy(false);
                    match res {
                        Ok(()) => {
                            {
                                let mut c = controls.lock().unwrap();
                                c.kindle_kokoro = desired;
                                c.save();
                            }
                            ui.set_kindle_kokoro(desired);
                            ui.set_status(
                                format!(
                                    "Kindle voice set to {}. Reopen Kindle for it to take effect.",
                                    if desired { "Kokoro" } else { "Microsoft David" }
                                )
                                .into(),
                            );
                        }
                        Err(e) => {
                            ui.set_kindle_kokoro(!desired); // revert the checkbox
                            ui.set_status(e.into());
                        }
                    }
                });
            });
        });
    }

    // --- Preview (synth via the host pipe) ---
    {
        let ui_weak = ui.as_weak();
        let controls = controls.clone();
        let voices = voices.clone();
        ui.on_preview_clicked(move || {
            let already = ui_weak.upgrade().map(|ui| ui.get_previewing()).unwrap_or(true);
            if already {
                return;
            }
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_previewing(true);
                ui.set_status(slint::SharedString::new());
            }
            let text = intro_for(&controls.lock().unwrap().voice, &voices);
            let weak = ui_weak.clone();
            std::thread::spawn(move || {
                let res = preview::play(&text);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_previewing(false);
                    if let Err(e) = res {
                        ui.set_status(e.into());
                    }
                });
            });
        });
    }

    ui.run()
}
