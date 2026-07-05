// Native settings panel for Kokoro Kindle Reader (egui/eframe). Spawned on demand
// by the headless host's tray "Settings" item. It reads/writes the same
// controls.json the host reads per utterance/sub-frame, so a narrator/speed/gain/
// chunk change lands on Kindle's next page. This is M3a: the controls UI. Model
// download/verify, the Kindle-voice toggle, and Preview (via the pipe) come next.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use eframe::egui;

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

struct PanelApp {
    voices: Vec<Voice>,
    controls: Controls,
    dirty: bool,
    status: String,
}

impl PanelApp {
    fn new() -> Self {
        let controls = Controls::load();
        PanelApp {
            voices: load_voices(),
            controls,
            dirty: false,
            status: String::new(),
        }
    }

    fn selected_name(&self) -> String {
        self.voices
            .iter()
            .find(|v| v.id == self.controls.voice)
            .map(|v| format!("{} ({})", v.name, v.group))
            .unwrap_or_else(|| self.controls.voice.clone())
    }
}

impl eframe::App for PanelApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Kokoro Kindle Reader");
            ui.label("Settings apply to Kindle narration on the next page.");
            ui.add_space(8.0);

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

            ui.add_space(10.0);
            ui.separator();
            ui.label(
                egui::RichText::new(
                    "Model download, Kindle-voice toggle, and Preview coming next.",
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
