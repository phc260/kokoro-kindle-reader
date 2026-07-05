// Native settings panel for Kokoro Kindle Reader (egui/eframe). Spawned on demand
// by the headless host's tray "Settings" item. It reads/writes the same
// controls.json the host reads per utterance/sub-frame, so a narrator/speed/gain/
// chunk change lands on Kindle's next page. This is M3a: the controls UI. Model
// download/verify, the Kindle-voice toggle, and Preview (via the pipe) come next.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

mod download;
mod kindle;
mod preview;

// Same identifier as the host / Tauri app: controls.json lives under
// %APPDATA%\<identifier>.
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
// Embedded so the narrator list stays in sync with what actually downloads.
const MANIFEST_JSON: &str = include_str!("../../src-tauri/model-manifest.json");
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

/// Pretty display name from an id: "af_heart" -> "Heart", "am_mc" -> "Mc".
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

/// Voices, in manifest order (which is already grouped af/am/bf/bm).
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

/// The persisted settings (controls.json). Mirrors native_synth::read_controls +
/// the Kindle-voice flag; unknown/missing keys fall back to these defaults.
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

    fn save(&self) -> std::io::Result<()> {
        let dir = app_data_dir();
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::json!({
            "voice": self.voice,
            "speed": self.speed,
            "gain": self.gain,
            "chunk": self.chunk,
            "kindle_kokoro": self.kindle_kokoro,
        });
        let txt = serde_json::to_string_pretty(&json).unwrap_or_default();
        std::fs::write(dir.join("controls.json"), txt)
    }
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

struct PanelApp {
    app_data: PathBuf,
    voices: Vec<Voice>,
    controls: Controls,
    dirty: bool,
    status: String,
    model_ready: bool,
    // Preview runs on a background thread (pipe client + rodio); these are shared
    // with it so the UI can disable the button and surface any error.
    preview_running: Arc<AtomicBool>,
    preview_msg: Arc<Mutex<String>>,
    // Model download + verify (both blocking, on background threads).
    dl_running: Arc<AtomicBool>,
    dl_progress: Arc<Mutex<download::Progress>>,
    verify_running: Arc<AtomicBool>,
    verify_msg: Arc<Mutex<String>>,
    // Kindle-voice switch (elevated, on a background thread). Outcome carries the
    // applied value on success, or an error message.
    kindle_running: Arc<AtomicBool>,
    kindle_outcome: Arc<Mutex<Option<Result<bool, String>>>>,
}

impl PanelApp {
    fn new() -> Self {
        let app_data = app_data_dir();
        let controls = Controls::load();
        let model_ready = download::model_complete(&app_data);
        PanelApp {
            app_data,
            voices: load_voices(),
            controls,
            dirty: false,
            status: String::new(),
            model_ready,
            preview_running: Arc::new(AtomicBool::new(false)),
            preview_msg: Arc::new(Mutex::new(String::new())),
            dl_running: Arc::new(AtomicBool::new(false)),
            dl_progress: Arc::new(Mutex::new(download::Progress::default())),
            verify_running: Arc::new(AtomicBool::new(false)),
            verify_msg: Arc::new(Mutex::new(String::new())),
            kindle_running: Arc::new(AtomicBool::new(false)),
            kindle_outcome: Arc::new(Mutex::new(None)),
        }
    }

    /// Switch Kindle's default voice on a background thread (elevated; raises UAC).
    /// `desired` is the target of kindle_kokoro; committed to controls only on success.
    fn start_kindle(&self, desired: bool, ctx: egui::Context) {
        if self.kindle_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let running = self.kindle_running.clone();
        let outcome = self.kindle_outcome.clone();
        std::thread::spawn(move || {
            let res = kindle::set_voice(desired).map(|_| desired);
            *outcome.lock().unwrap() = Some(res);
            running.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn start_download(&self, ctx: egui::Context) {
        let repaint = move || ctx.request_repaint();
        download::start(
            self.app_data.clone(),
            self.dl_running.clone(),
            self.dl_progress.clone(),
            repaint,
        );
    }

    fn start_verify(&self, ctx: egui::Context) {
        if self.verify_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let running = self.verify_running.clone();
        let msg = self.verify_msg.clone();
        let app_data = self.app_data.clone();
        *msg.lock().unwrap() = String::new();
        std::thread::spawn(move || {
            let (checked, repaired) = download::verify(&app_data);
            *msg.lock().unwrap() = if repaired == 0 {
                format!("All {checked} files verified OK.")
            } else {
                format!("{repaired} of {checked} files were bad and removed — click Download to repair.")
            };
            running.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    fn selected_name(&self) -> String {
        self.voices
            .iter()
            .find(|v| v.id == self.controls.voice)
            .map(|v| format!("{} ({})", v.name, v.group))
            .unwrap_or_else(|| self.controls.voice.clone())
    }

    /// Kick off a preview on a background thread: synth the current voice's intro
    /// through the host pipe and play it. Errors surface in preview_msg.
    fn start_preview(&self, ctx: egui::Context) {
        if self.preview_running.swap(true, Ordering::SeqCst) {
            return; // already running
        }
        let text = intro_for(&self.controls.voice, &self.voices);
        let running = self.preview_running.clone();
        let msg = self.preview_msg.clone();
        *msg.lock().unwrap() = String::new();
        std::thread::spawn(move || {
            let result = preview::play(&text);
            *msg.lock().unwrap() = match result {
                Ok(()) => String::new(),
                Err(e) => e,
            };
            running.store(false, Ordering::SeqCst);
            ctx.request_repaint(); // wake the UI to re-enable the button
        });
    }
}

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Finalize a just-finished download and keep model_ready fresh — only when
        // idle, so we don't stat the model files during an active download.
        if !self.dl_running.load(Ordering::Relaxed) && !self.verify_running.load(Ordering::Relaxed) {
            let finished = {
                let mut p = self.dl_progress.lock().unwrap();
                if p.done {
                    p.done = false;
                    Some(p.error.take())
                } else {
                    None
                }
            };
            if let Some(err) = finished {
                self.status = match err {
                    Some(e) => format!("Download error: {e}"),
                    None => "Model downloaded.".to_string(),
                };
            }
            self.model_ready = download::model_complete(&self.app_data);
        }

        // Fold in a finished Kindle-voice switch: commit the applied value (and
        // persist it) on success, or surface the error.
        if let Some(res) = self.kindle_outcome.lock().unwrap().take() {
            match res {
                Ok(applied) => {
                    self.controls.kindle_kokoro = applied;
                    self.dirty = true;
                    self.status = format!(
                        "Kindle voice set to {}. Reopen Kindle for it to take effect.",
                        if applied { "Kokoro" } else { "Microsoft David" }
                    );
                }
                Err(e) => self.status = e,
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Kokoro Kindle Reader");
            ui.label("Settings apply to Kindle narration on the next page.");
            ui.add_space(8.0);

            // Model status / download.
            let dl = self.dl_running.load(Ordering::Relaxed);
            let vf = self.verify_running.load(Ordering::Relaxed);
            if dl {
                let p = self.dl_progress.lock().unwrap().clone();
                let frac = if p.total > 0 {
                    p.downloaded as f32 / p.total as f32
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(frac).show_percentage());
                ui.label(
                    egui::RichText::new(format!(
                        "Downloading {} — {:.0} / {:.0} MB",
                        p.file,
                        p.downloaded as f32 / 1e6,
                        p.total as f32 / 1e6
                    ))
                    .small()
                    .weak(),
                );
                ctx.request_repaint();
            } else if !self.model_ready {
                ui.colored_label(
                    egui::Color32::from_rgb(0xc0, 0x90, 0x30),
                    "Model not downloaded (~420 MB).",
                );
                if ui.button("Download model").clicked() {
                    self.start_download(ctx.clone());
                }
            } else {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(0x40, 0xa0, 0x40), "● Model ready");
                    ui.add_enabled_ui(!vf, |ui| {
                        if ui.small_button("Verify & repair").clicked() {
                            self.start_verify(ctx.clone());
                        }
                    });
                    if vf {
                        ui.spinner();
                        ctx.request_repaint();
                    }
                });
                let vmsg = self.verify_msg.lock().unwrap().clone();
                if !vmsg.is_empty() {
                    ui.label(egui::RichText::new(vmsg).small().weak());
                }
            }
            ui.separator();
            ui.add_space(6.0);

            // Narrator — grouped dropdown.
            ui.horizontal(|ui| {
                ui.label("Narrator");
                egui::ComboBox::from_id_salt("narrator")
                    .selected_text(self.selected_name())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        let mut last_group = String::new();
                        // Borrow voices immutably while mutating controls.voice.
                        for v in &self.voices {
                            if v.group != last_group {
                                if !last_group.is_empty() {
                                    ui.separator();
                                }
                                ui.label(egui::RichText::new(&v.group).small().weak());
                                last_group = v.group.clone();
                            }
                            if ui
                                .selectable_label(self.controls.voice == v.id, &v.name)
                                .clicked()
                            {
                                self.controls.voice = v.id.clone();
                                self.dirty = true;
                            }
                        }
                    });
            });
            ui.add_space(6.0);

            // Speed / gain / chunk.
            if ui
                .add(egui::Slider::new(&mut self.controls.speed, 0.5..=2.0).text("Speed"))
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .add(egui::Slider::new(&mut self.controls.gain, 0.0..=2.0).text("Volume (gain)"))
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .add(
                    egui::Slider::new(&mut self.controls.chunk, 1..=8)
                        .text("Sentences per chunk"),
                )
                .changed()
            {
                self.dirty = true;
            }

            ui.add_space(8.0);
            // Kindle default-voice toggle (elevated; one UAC prompt).
            let kindle_running = self.kindle_running.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                let mut kk = self.controls.kindle_kokoro;
                ui.add_enabled_ui(!kindle_running, |ui| {
                    // Render from controls; on click, kick off the elevated switch —
                    // controls.kindle_kokoro is only committed once it succeeds.
                    if ui
                        .checkbox(&mut kk, "Use Kokoro as Kindle's default voice")
                        .changed()
                    {
                        self.start_kindle(kk, ctx.clone());
                    }
                });
                if kindle_running {
                    ui.spinner();
                    ctx.request_repaint();
                }
            });

            ui.add_space(10.0);
            // Preview — synthesize this voice's intro through the host (WYSIWYG).
            let running = self.preview_running.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!running, |ui| {
                    if ui.button("▶  Preview voice").clicked() {
                        self.start_preview(ctx.clone());
                    }
                });
                if running {
                    ui.spinner();
                    ui.label("Synthesizing…");
                    ctx.request_repaint();
                }
            });
            let pmsg = self.preview_msg.lock().unwrap().clone();
            if !pmsg.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(0xd0, 0x60, 0x60), pmsg);
            }

            ui.add_space(10.0);
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Kokoro Kindle Reader must stay running (tray) for Kindle to narrate.",
                )
                .small()
                .weak(),
            );

            if !self.status.is_empty() {
                ui.add_space(6.0);
                ui.label(&self.status);
            }
        });

        // Persist any change immediately (tiny file; the host re-reads it per page).
        if self.dirty {
            self.dirty = false;
            match self.controls.save() {
                Ok(()) => self.status = "Saved.".to_string(),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 460.0])
            .with_title("Kokoro Kindle Reader — Settings"),
        ..Default::default()
    };
    eframe::run_native(
        "kokoro-panel",
        native_options,
        Box::new(|_cc| Ok(Box::new(PanelApp::new()))),
    )
}
