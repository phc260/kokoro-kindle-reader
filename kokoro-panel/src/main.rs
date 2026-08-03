// Native settings panel for Kokoro Kindle Reader (Slint / Fluent theme). Spawned on
// demand by the headless host's tray "Settings" item. Reads/writes the same
// controls.json the host reads per utterance/sub-frame, so a narrator/speed/gain/
// chunk change lands on Kindle's next page. Model download/verify, the Kindle-voice
// toggle, and Preview (synth via the host pipe = WYSIWYG) are all here.
//
// It does NOT touch Kindle. `kokoro-host` is the sole authority for Kindle reading and
// health: Play/Stop/Pause/Resume go over the named pipe as intent (`hostlink`), and the
// panel draws the state the host reports back — including a ~1 Hz heartbeat over the same
// pipe, whose failure to connect is what "host offline" means. What the panel still owns is
// its own persisted settings (controls.json) and its own audio (Preview).
//
// The UI is declared in ui/panel.slint (compiled by build.rs); this file wires its
// properties/callbacks to the framework-agnostic logic in download.rs / preview.rs /
// hostlink.rs / benchmark.rs. Background work (download, verify, preview, every host
// request) runs on threads and pushes results back via `upgrade_in_event_loop`; nothing
// blocking runs on the Slint UI thread.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use kokoro_protocol::{KINDLE_CLOSE, KINDLE_PAUSE, KINDLE_PLAY, KINDLE_QUERY, KINDLE_RESUME, KINDLE_STOP};

mod benchmark;
mod download;
mod hostlink;
mod preview;

slint::include_modules!();

// Same identifier as the host: controls.json lives under %APPDATA%\<identifier>.
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
// Embedded so the narrator list stays in sync with what actually downloads.
const MANIFEST_JSON: &str = include_str!("../../model-manifest.json");
const DEFAULT_VOICE: &str = "af_heart";
/// Ceiling on the lock when turning Read Aloud **on**. Only reached when the flip never
/// lands: Kindle with no book open, a Ctrl+A swallowed by something, a belief that was already
/// wrong. Generous because what it waits on is synthesis, and the first chunk of a page can
/// take double digits of seconds on a machine synthesizing slower than real time. Without a
/// cap at all, a flip that cannot take effect would disable the switch for the rest of the
/// session — worse than the race this replaced, because there would be no way to retry.
const SETTLE_CAP_ON: Duration = Duration::from_secs(15);
/// Ceiling on the lock when turning Read Aloud **off**. Much shorter, because the only thing
/// outstanding is audio draining — stream lead plus debounce, ~2.5 s — and unlike the "on"
/// direction the host's belief is already correct the moment the command returns, so a second
/// flip from here is safe whether or not the tail has finished. The cap's real job is to stop
/// Kindle *choosing to play out the rest of the page* from holding the switch for the length
/// of that page.
const SETTLE_CAP_OFF: Duration = Duration::from_secs(8);
/// How often to ask the host while a flip is settling. Much faster than the idle heartbeat:
/// this is the one stretch where the answer changes something the user is looking at, and
/// `KINDLE_QUERY` is answered inline off atomics, so it costs the host nothing to ask.
const SETTLE_POLL: Duration = Duration::from_millis(250);

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

/// A short self-introduction spoken as the preview sample.
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

// --- GPU-vs-CPU speed test -------------------------------------------------
// "Synthesize on GPU" asks the user to answer a question they have no way to answer:
// on one laptop the integrated GPU runs at half the CPU's rate, on the next machine
// it's several times faster, and the hardware name doesn't tell you which. So the panel
// measures it — kokoro-bench, which settled the same question during development, but
// wired to a dialog and run against the engine the user actually has installed.

/// A measured engine, as the results row shows it.
fn speed_text(s: Option<benchmark::Speed>) -> String {
    match s {
        Some(s) => format!("{:.1}× real time", s.realtime),
        None => "unavailable".to_string(),
    }
}

/// Below this ratio the two engines are called a tie: a single timed run each can't
/// resolve a few percent, and flipping the user's engine on that noise would be
/// pretending to a precision the test doesn't have.
const BENCH_TIE_RATIO: f32 = 1.05;

/// Show what's being timed right now.
fn bench_phase(weak: &slint::Weak<AppWindow>, msg: &str) {
    let msg = msg.to_string();
    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_bench_phase(msg.into()));
}

/// Give up on the test: close the dialog and explain in the panel's status line (where
/// the rest of the panel's failures are reported), changing no setting.
fn bench_abort(weak: &slint::Weak<AppWindow>, status: String) {
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_bench_running(false);
        ui.set_bench_visible(false);
        ui.set_status(status.into());
    });
}

/// Time both engines and tick the winner. Blocking (tens of seconds per engine) — runs
/// on a background thread. `cancel` is checked between engines, which is as often as it
/// can be: a run in flight holds the host's synth worker until it finishes.
fn run_speed_test(
    weak: slint::Weak<AppWindow>,
    cancel: Arc<AtomicBool>,
    controls: Arc<Mutex<Controls>>,
) {
    // Gate on `any_speaking`, not `kindle_speaking` (see its doc): starting a measurement
    // mid-narration would stall that narration AND time the contention rather than the
    // engine. Only the *wording* below narrows to Kindle, because that's the one the user
    // can act on from here. Best-effort either way — nothing reserves the worker, so a page
    // can begin in the instant after this reads idle; the cost of guessing wrong is a click
    // one way and a ~40 s benchmark in front of live narration the other.
    match hostlink::send(KINDLE_QUERY, hostlink::ACTION_BUSY_WAIT) {
        Err(e) => return bench_abort(&weak, format!("Speed test failed: {e}")),
        Ok(st) if st.any_speaking => {
            return bench_abort(
                &weak,
                if st.kindle_speaking {
                    "Kokoro is reading in Kindle right now — stop Read Aloud, then run the speed test."
                } else {
                    "Kokoro is speaking right now — wait for it to finish, then run the speed test."
                }
                .to_string(),
            )
        }
        Ok(_) => {}
    }

    // GPU first (the phase label for it is set by the click handler, so the dialog never
    // paints an empty line before this thread gets going).
    //
    // Known skew, accepted: on a laptop where the iGPU and the CPU cores share one
    // package power budget (see kokoro-bench), the CPU is measured on a warmer package
    // than the GPU was, so a near-tie can tilt toward whichever runs first. Every remedy
    // costs more than it buys — a cooldown adds dead time to a modal the user is already
    // waiting at, and reversing or shuffling the order just moves the bias. The tie band
    // below absorbs the small differences this can produce; a gap big enough to matter is
    // bigger than thermals explain.
    let gpu = match benchmark::measure(true) {
        Ok(v) => v,
        Err(e) => return bench_abort(&weak, format!("Speed test failed: {e}")),
    };
    if cancel.load(Ordering::SeqCst) {
        return bench_abort(&weak, "Speed test stopped; the setting is unchanged.".to_string());
    }
    bench_phase(&weak, "Timing the processor — 2 of 2…");
    let cpu = match benchmark::measure(false) {
        Ok(v) => v,
        Err(e) => return bench_abort(&weak, format!("Speed test failed: {e}")),
    };
    if cancel.load(Ordering::SeqCst) {
        return bench_abort(&weak, "Speed test stopped; the setting is unchanged.".to_string());
    }

    // `select` is None wherever the measurement doesn't justify touching the user's
    // setting — a tie, or a test that measured nothing at all.
    let (gpu_wins, cpu_wins, verdict, select) = match (gpu, cpu) {
        (Some(g), Some(c)) => {
            let gpu_faster = g.realtime >= c.realtime;
            let (fast, slow) = if gpu_faster {
                (g.realtime, c.realtime)
            } else {
                (c.realtime, g.realtime)
            };
            let ratio = fast / slow.max(f32::MIN_POSITIVE);
            if ratio < BENCH_TIE_RATIO {
                (false, false, "Both engines run at about the same speed here, so the setting is unchanged.".to_string(), None)
            } else {
                let name = if gpu_faster { "The graphics card" } else { "The processor" };
                let mut v = format!("{name} is {ratio:.1}× faster here — now selected.");
                if fast < 1.0 {
                    v.push_str(
                        " Even so, it synthesizes slower than it speaks on this PC, so expect reading to pause to catch up.",
                    );
                }
                (gpu_faster, !gpu_faster, v, Some(gpu_faster))
            }
        }
        (Some(_), None) => (
            true,
            false,
            "Only the graphics card could synthesize on this PC — now selected.".to_string(),
            Some(true),
        ),
        (None, Some(_)) => (
            false,
            true,
            "The graphics card can't synthesize on this PC — switched to the processor."
                .to_string(),
            Some(false),
        ),
        (None, None) => (
            false,
            false,
            "Neither engine could run the test — check that the model finished downloading. The setting is unchanged."
                .to_string(),
            None,
        ),
    };

    if let Some(want) = select {
        let mut c = controls.lock().unwrap();
        c.gpu_synth = want;
        c.save();
    }

    let gpu_text = speed_text(gpu);
    let cpu_text = speed_text(cpu);
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_bench_gpu_text(gpu_text.into());
        ui.set_bench_cpu_text(cpu_text.into());
        ui.set_bench_gpu_wins(gpu_wins);
        ui.set_bench_cpu_wins(cpu_wins);
        ui.set_bench_verdict(verdict.into());
        ui.set_bench_running(false);
        ui.set_bench_done(true);
        // Mirror the applied engine onto the checkbox (two-way bound, so this moves the
        // widget too). Setting the property doesn't re-fire `toggled`, so it won't
        // bounce back through gpu-synth-changed and re-save.
        if let Some(want) = select {
            ui.set_gpu_synth(want);
        }
    });
}

/// The persisted settings (controls.json).
///
/// Settings only. Pause used to live here too, which made a *command* travel as a file the
/// host polled; it is now host-owned live state reached over the pipe, so nothing in this
/// struct is a live instruction to anybody.
struct Controls {
    voice: String,
    speed: f32,
    gain: f32,
    chunk: u32,
    kindle_kokoro: bool,
    // Manual GPU/CPU escape hatch (see the speed test above). Default true = GPU.
    gpu_synth: bool,
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            voice: DEFAULT_VOICE.to_string(),
            speed: 1.0,
            gain: 1.0,
            chunk: 2,
            kindle_kokoro: true,
            gpu_synth: true,
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
                if let Some(x) = v.get("gpu_synth").and_then(|x| x.as_bool()) {
                    c.gpu_synth = x;
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
            "gpu_synth": self.gpu_synth,
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
        ui.set_gpu_synth(c.gpu_synth);
    }
    // Reading/pause/host-health all arrive from the host's heartbeat. Start pessimistic:
    // until one reply has landed the panel knows nothing, and claiming "ready" for the
    // ~1 s before it does is exactly the false confidence the heartbeat exists to remove.
    ui.set_host_online(false);
    ui.set_model_ready(download::model_complete(&app_data));
    // "~N MB" from the manifest sum, same decimal-MB (/1e6) convention as the live
    // download counter below — never hardcode the size, so it tracks the manifest.
    ui.set_model_size_label(format!("~{:.0} MB", download::total_bytes() as f32 / 1e6).into());

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
    // Narrator: all three dropdowns commit the resulting voice to controls.json and re-warm
    // the preview buffer for it. One body, three registrations — accent and gender differ
    // only in re-filtering the name list first (reset to its first entry).
    let on_narrator = {
        let weak = ui.as_weak();
        let voices = voices.clone();
        let controls = controls.clone();
        let cache = preview_cache.clone();
        let gen = preview_gen.clone();
        move |relist: bool| {
            let Some(ui) = weak.upgrade() else { return };
            if relist {
                refilter(&ui, &voices, None);
            }
            commit_voice(&ui, &voices, &controls);
            prefetch_for_current(&ui, &voices, &cache, &gen);
        }
    };
    ui.on_accent_changed({
        let f = on_narrator.clone();
        move |_| f(true)
    });
    ui.on_gender_changed({
        let f = on_narrator.clone();
        move |_| f(true)
    });
    ui.on_name_changed(move |_| on_narrator(false));
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
    {
        let controls = controls.clone();
        ui.on_gpu_synth_changed(move |v| {
            let mut c = controls.lock().unwrap();
            c.gpu_synth = v;
            c.save();
        });
    }

    // --- GPU-vs-CPU speed test (dialog) ---
    // `running` outlives the dialog: Stop only takes effect between engines, so the
    // worker can still be finishing a run after the user has dismissed the card.
    let bench_running = Arc::new(AtomicBool::new(false));
    let bench_cancel = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let running = bench_running.clone();
        ui.on_bench_open(move || {
            if let Some(ui) = weak.upgrade() {
                let busy = running.load(Ordering::SeqCst);
                ui.set_bench_running(busy); // reopening mid-test shows the progress face
                ui.set_bench_done(false);
                ui.set_bench_visible(true);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_bench_close(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_bench_visible(false);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let cancel = bench_cancel.clone();
        ui.on_bench_stop(move || {
            cancel.store(true, Ordering::SeqCst);
            if let Some(ui) = weak.upgrade() {
                // The dialog stays up until the engine mid-run releases the host, so
                // say so rather than appearing to have ignored the click.
                ui.set_bench_phase("Stopping after this engine finishes…".into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let controls = controls.clone();
        let running = bench_running.clone();
        let cancel = bench_cancel.clone();
        ui.on_bench_start(move || {
            if running.swap(true, Ordering::SeqCst) {
                return; // a previous test is still finishing
            }
            cancel.store(false, Ordering::SeqCst);
            if let Some(ui) = weak.upgrade() {
                ui.set_bench_running(true);
                ui.set_bench_done(false);
                ui.set_bench_phase("Timing the graphics card — 1 of 2…".into());
                ui.set_status(slint::SharedString::new());
            }
            let weak = weak.clone();
            let controls = controls.clone();
            let running = running.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                run_speed_test(weak, cancel, controls);
                running.store(false, Ordering::SeqCst);
            });
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
                // `total` is a sum of file sizes, so zero here means a manifest whose files
                // are all zero-byte — not an empty one, which never reaches this callback at
                // all (`download::verify` only calls it from inside its per-file loop). Every
                // such file is trivially verified, so the answer is 100%, and it is decided
                // once here because the bar and the whole-percent gating it used to disagree:
                // `pct` said 100 while `frac` said 0.0.
                let (pct, frac) = match total {
                    0 => (100, 1.0),
                    t => ((done * 100 / t) as i32, done as f32 / t as f32),
                };
                if pct != last_pct {
                    last_pct = pct;
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

    // Every command this panel sends the host goes through here, in click order. Its
    // `busy()` is set while one is in flight: the heartbeat keeps running underneath (that's
    // the point of a 1 Hz health check), but it must not repaint the switch from state that
    // predates a command already on its way — the host reports its own `busy` for the window
    // it knows about, and this covers the moment before it does.
    let intents = Intents::spawn(ui.as_weak());
    // Holds the Read Aloud switch from a flip until the host demonstrates it landed.
    let settling = Settling::new();

    // --- Kindle-voice toggle (confirm, persist, close Kindle) ---
    // The `kindle_kokoro` flag only lands on Kindle's next launch (the host's watcher
    // injects — or doesn't — the hook then), so a click first raises a Yes/No dialog:
    // Yes persists the flag and closes Kindle (the user reopens it, picking up the
    // change); No reverts the checkbox and persists nothing.
    {
        let ui_weak = ui.as_weak();
        ui.on_kindle_toggled(move |desired| {
            // The checkbox already flipped optimistically (two-way binding); hold the
            // desired value in the dialog, unpersisted, until the user confirms.
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_confirm_target(desired);
                ui.set_confirm_visible(true);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let controls = controls.clone();
        let intents = intents.clone();
        ui.on_confirm_kindle(move |accepted| {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.set_confirm_visible(false);
            let desired = ui.get_confirm_target();
            if !accepted {
                ui.set_kindle_kokoro(!desired); // undo the optimistic checkbox flip
                return;
            }
            {
                let mut c = controls.lock().unwrap();
                c.kindle_kokoro = desired;
                c.save();
            }
            ui.set_status(
                if desired {
                    "Kokoro will narrate Kindle. Closing Kindle..."
                } else {
                    "Kindle will use its own voice. Closing Kindle..."
                }
                .into(),
            );
            // Ask the HOST to close Kindle — it owns every interaction with Kindle, and it
            // is also the thing that will (or won't) inject the hook on the next launch, so
            // the process that acts on the flag is the one that acts on the window. Queued
            // with the transport's commands rather than beside them: closing Kindle and
            // driving its reader are the same host thread, so they must not overtake.
            intents.send(KINDLE_CLOSE, |ui, res| {
                ui.set_status(
                    match res {
                        Ok(st) if st.ok => st.message,
                        // A host that answered but couldn't do it says why; a host that
                        // didn't answer is offline. Either way the flag is saved, so the
                        // change still lands on Kindle's next launch.
                        Ok(st) => {
                            format!("{} The change applies the next time Kindle opens.", st.message)
                        }
                        Err(e) => format!("{e} The change applies the next time Kindle opens."),
                    }
                    .into(),
                );
            });
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
                // Preview is the one sound the panel makes itself, so it's the one the
                // host can't report; set the indicator here rather than leaving it up to
                // a heartbeat that could be most of a second away.
                ui.set_speaking(true);
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
                    // Preview only runs while Read Aloud is off, so nothing else of ours is
                    // sounding; the next heartbeat re-derives this either way.
                    ui.set_speaking(false);
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

    // --- Read Aloud (ask the host to start/stop Kindle's Assistive reader) ---
    // The switch already flipped `reading` optimistically; `want` is that new value.
    {
        let ui_weak = ui.as_weak();
        let intents = intents.clone();
        let settling = settling.clone();
        let active_sink = active_sink.clone();
        ui.on_read_aloud_clicked(move |want| {
            // Toggling the reader hands the transport over to Kindle, so silence any
            // preview still playing — its thread then clears `previewing`. Otherwise a
            // preview started before Read Aloud would keep playing over Kindle's
            // narration with its Stop button hidden by the reading-state view.
            preview::stop(&active_sink);
            if let Some(ui) = ui_weak.upgrade() {
                // Starting or stopping clears a pause already in effect. The host does this
                // itself (it owns the flag); this is only the switch catching up at once
                // instead of a heartbeat later. A pause the user issues *after* this, while
                // the Play is still driving Kindle, is honoured — see `apply_kindle`.
                ui.set_paused(false);
                ui.set_status(slint::SharedString::new());
                // Hold the switch until the host shows this flip took effect, so it can't be
                // flipped again while the first one is still working its way through Kindle.
                // The heartbeat speeds up to `SETTLE_POLL` while this is pending and releases
                // the lock the moment the host's report agrees.
                settling.begin(want);
                ui.set_read_aloud_locked(true);
            }
            let action = if want { KINDLE_PLAY } else { KINDLE_STOP };
            intents.send(
                action,
                paint_transport(settling.clone(), move |ui| ui.set_reading(!want)),
            );
        });
    }

    // --- Pause / Resume (ask the host to stall its audio stream mid-page) ---
    // The host owns the flag and reads it per sub-frame, so the stream stalls where it is
    // and Kindle keeps the page. Cheap on the host side (it flips an atomic and answers
    // inline), but still off the UI thread because it's pipe I/O.
    {
        let ui_weak = ui.as_weak();
        let intents = intents.clone();
        let settling = settling.clone();
        ui.on_pause_toggled(move |want| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_paused(want); // optimistic; the reply confirms or corrects it
            }
            let action = if want { KINDLE_PAUSE } else { KINDLE_RESUME };
            intents.send(
                action,
                paint_transport(settling.clone(), move |ui| ui.set_paused(!want)),
            );
        });
    }

    // --- Host heartbeat (1 Hz over the pipe) ---
    start_heartbeat(ui.as_weak(), intents.clone(), settling.clone());

    ui.run()
}

/// What the host said, or why it couldn't be asked. The `Err` side is a message for the
/// user, not a diagnostic: an unreachable host is the ordinary case (it is a tray daemon the
/// user can quit), and it is the one answer no report field can express.
type HostReply = Result<hostlink::HostReport, String>;

/// The UI-thread half of an [`Intent`] — boxed because the queue holds commands of different
/// shapes in one channel.
type PaintReply = Box<dyn FnOnce(&AppWindow, HostReply) + Send>;

/// One queued command for the host, with the painter for whatever comes back.
struct Intent {
    action: u8,
    paint: PaintReply,
}

/// Sends every command this panel issues to the host, **in the order the user issued them**,
/// one at a time, off the UI thread.
///
/// ONE long-lived thread — not a thread per click, and not a lane per kind of command. Both
/// alternatives lose the ordering, and the ordering is the whole point: each command would
/// get its own pipe connection, connections are served in whatever order they arrive, and the
/// host's `KindleCtl` serializes by arrival rather than by when the user clicked. Every such
/// inversion is a lasting wrong state, because these commands are not idempotent — a Stop
/// that overtakes its own Play finds reading already off, no-ops, and leaves the delayed Play
/// to start reading with the switch saying stopped; a Pause that arrives after the Stop it
/// preceded parks a stream that the panel, believing reading is off, offers no Resume for.
/// Against a blind Ctrl+A toggle these are unrecoverable until the user notices and clicks
/// again. Splitting Pause/Resume onto a second lane fixes the latency below and reintroduces
/// exactly these two races across the lanes, so: one queue, and the last click wins.
///
/// The cost is that a Pause can wait out a Play, Stop or Close ahead of it — seconds of UI
/// Automation. That reads like a breach of "a pause must never queue behind Kindle work", and
/// isn't: that invariant is about the *host's* control thread, and the case it protects is a
/// pause landing mid-page while Kindle narrates. This queue is empty then. It is non-empty
/// only while a Play, Stop or Close is in flight — and in that window there is either nothing
/// being narrated yet (Play) or narration the queued command is about to end anyway (Stop,
/// Close). The heartbeat's query never enters this queue at all: it has its own thread, so
/// health stays prompt no matter what is sitting in here.
#[derive(Clone)]
struct Intents {
    tx: mpsc::Sender<Intent>,
    /// Commands queued but not yet *painted* — the count is released inside the UI closure,
    /// not when the reply is merely posted.
    pending: Arc<AtomicUsize>,
    /// Bumped once per command queued, and never reset.
    ///
    /// `pending` answers "is one in flight right now"; this answers "did one happen at all
    /// since I last looked", and only the second is safe against a badly-timed deschedule. A
    /// command can be queued, complete, and drain `pending` back to zero entirely inside the
    /// gap between the heartbeat receiving its reply and posting the closure that draws it —
    /// leaving that tick to repaint the switch from a report older than the click, with
    /// nothing in the counter left to say so. The heartbeat samples this before it queries
    /// and again as it paints; any change means the report in hand predates a click.
    epoch: Arc<AtomicU64>,
}

impl Intents {
    fn spawn(weak: slint::Weak<AppWindow>) -> Intents {
        let (tx, rx) = mpsc::channel::<Intent>();
        let pending = Arc::new(AtomicUsize::new(0));
        {
            let pending = pending.clone();
            std::thread::spawn(move || {
                for Intent { action, paint } in rx {
                    let res = hostlink::send(action, hostlink::ACTION_BUSY_WAIT);
                    let done = pending.clone();
                    let posted = weak.upgrade_in_event_loop(move |ui| {
                        paint(&ui, res);
                        done.fetch_sub(1, Ordering::SeqCst);
                    });
                    if posted.is_err() {
                        // The closure will never run, so release the count here instead.
                        pending.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            });
        }
        Intents { tx, pending, epoch: Arc::new(AtomicU64::new(0)) }
    }

    /// Queue one command. Returns at once; `paint` runs on the UI thread with the reply.
    fn send(
        &self,
        action: u8,
        paint: impl FnOnce(&AppWindow, HostReply) + Send + 'static,
    ) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Intent { action, paint: Box::new(paint) }).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Is a command queued or mid-paint?
    fn busy(&self) -> bool {
        self.pending.load(Ordering::SeqCst) != 0
    }

    /// How many commands have been queued this session.
    fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

/// A Read Aloud flip the user has made and the host has not yet shown the effect of.
struct Transition {
    /// What was asked for: true = start reading.
    want: bool,
    /// When it was asked. The cap ([`SETTLE_CAP_ON`] / [`SETTLE_CAP_OFF`]) runs from here.
    since: Instant,
}

/// Holds the Read Aloud switch locked from a flip until the host *demonstrates* the flip
/// happened — replacing a fixed timer, which could only ever be a guess at a duration the
/// host actually knows.
///
/// The evidence is the host's own report, and it is symmetric. Turning on settles when the
/// host is visibly engaged with Kindle's audio: `kindle_synth` (a page in flight) or
/// `kindle_speaking` (audio out). Turning off settles when it is visibly neither. Both also
/// require the host's `busy` to have cleared, or a Stop would settle on the silence that its
/// own Ctrl+A has not yet caused.
///
/// `kindle_synth` is what makes the "on" direction work at all. Flipping the switch on takes
/// seconds of UI Automation, and then *more* seconds of synthesis before a single sample
/// exists — a stretch in which every audio clock reads idle and the old fixed timer expired
/// squarely in the middle. A clock cannot report work that has produced no audio yet; that
/// bit can, which is why the host now sends it.
#[derive(Clone)]
struct Settling(Arc<Mutex<Option<Transition>>>);

impl Settling {
    fn new() -> Settling {
        Settling(Arc::new(Mutex::new(None)))
    }

    /// Record a flip. The switch stays locked until [`Self::observe`] sees it land.
    fn begin(&self, want: bool) {
        *self.0.lock().unwrap() = Some(Transition { want, since: Instant::now() });
    }

    /// Give up waiting (the host went away; there is nothing left to settle against).
    fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }

    /// Is a flip still waiting on the host? Drives the faster poll cadence.
    fn pending(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    /// Feed one host report, with the instant it was *received*. Returns whether the switch
    /// should still be locked.
    ///
    /// `captured` is load-bearing. A heartbeat's reply can be sampled before the user clicks
    /// and painted after, and such a report describes the world before the flip — for a Stop
    /// issued between streams it reads "not busy, not engaged", which is precisely the shape
    /// of a Stop that has already landed. Acting on it releases the switch while the Ctrl+A
    /// is still sitting in the queue.
    fn observe(&self, st: &hostlink::HostReport, captured: Instant) -> bool {
        let mut held = self.0.lock().unwrap();
        let Some(t) = held.as_ref() else { return false };
        // The cap is checked first, so a stale report can't keep the lock alive past it.
        let cap = if t.want { SETTLE_CAP_ON } else { SETTLE_CAP_OFF };
        if t.since.elapsed() >= cap {
            *held = None;
            return false;
        }
        if captured <= t.since {
            return true; // predates the flip: it cannot describe the flip's effect
        }
        // Kindle went away: nothing can land, so stop waiting for it rather than holding the
        // lock out to the cap. The switch is dark anyway while `kindle-running` is false —
        // this is about the state it comes back in when Kindle reopens.
        if !st.kindle_running {
            *held = None;
            return false;
        }
        let engaged = st.kindle_synth || st.kindle_speaking;
        let landed = !st.busy && if t.want { engaged } else { !engaged };
        if landed {
            *held = None;
            return false;
        }
        true
    }
}

/// The transport's painter: adopt the host's report, which is authoritative either way — on
/// success it's the state the host applied, on failure the state Kindle was left in.
///
/// Only an unreachable host needs `revert`: the host vanished mid-command, so the switch's
/// optimistic flip has to be undone rather than left claiming a reader nothing is behind.
fn paint_transport(
    settling: Settling,
    revert: impl FnOnce(&AppWindow) + Send + 'static,
) -> impl FnOnce(&AppWindow, HostReply) + Send + 'static {
    move |ui, res| match res {
        Ok(st) => {
            apply_report(ui, &st);
            // This reply is the flip's own command answering (or a later one), so it is by
            // construction newer than the transition it is being judged against.
            ui.set_read_aloud_locked(settling.observe(&st, Instant::now()));
            ui.set_status(st.message.into());
        }
        Err(e) => {
            // Nothing left to settle against, and the switch is about to be disabled by
            // `reading-active` anyway — don't leave a lock behind for the host's return.
            settling.clear();
            revert(ui);
            ui.set_host_online(false);
            ui.set_read_aloud_locked(false);
            ui.set_status(e.into());
        }
    }
}

/// Mirror one host report onto the panel. The host is the authority for all three of these,
/// so there is nothing to merge — only `speaking` is OR'd with the panel's own Preview,
/// which is the one sound the host doesn't produce for anybody but us.
fn apply_report(ui: &AppWindow, st: &hostlink::HostReport) {
    ui.set_host_online(true);
    ui.set_kindle_running(st.kindle_running);
    ui.set_reading(st.reading);
    ui.set_paused(st.paused);
    ui.set_speaking(ui.get_previewing() || st.kindle_speaking);
}

/// Paint "the host isn't there". Also the panel's opening state, before the first reply:
/// not yet told and offline are the same picture, and the honest one to show for the
/// fraction of a second between the two.
///
/// Setting these properties from Rust doesn't fire the widgets' `toggled` callbacks, so
/// none of this loops back out as a command to a host that isn't listening.
fn apply_offline(ui: &AppWindow) {
    ui.set_host_online(false);
    // Only the host looks for Kindle, so with the host gone this is unknown, not false.
    // "Kindle isn't open" is suppressed while offline anyway; don't assert it.
    ui.set_kindle_running(false);
    // Nothing is narrating without a host, whatever the switch said a moment ago.
    ui.set_reading(false);
    ui.set_paused(false);
    ui.set_speaking(ui.get_previewing());
}

/// How long a heartbeat waits for its reply before calling the host offline. Comfortably
/// inside [`HEARTBEAT_PERIOD`], so a slow answer can't push the next check late.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(800);
/// Target heartbeat cadence: ~1 Hz while the panel is open.
const HEARTBEAT_PERIOD: Duration = Duration::from_millis(1000);

/// Poll the host's health for as long as the panel is open, and paint the result.
///
/// Health is proven by real I/O every time — a `CMD_KINDLE` query down the pipe. Nothing
/// here infers it from a process name, the tray icon, a cached voice list, a `controls.json`
/// timestamp, a Kindle window, or a request that succeeded a second ago; each of those
/// would keep reporting "ready" after the host had gone.
///
/// Exactly one request is in flight at a time. The request runs on a short-lived thread so
/// the wait can be bounded (a blocking pipe read has no timeout of its own); when that
/// bound expires we paint offline and then *wait out* the orphan before starting another,
/// so a wedged host leaves one stuck thread rather than a new one every second.
fn start_heartbeat(weak: slint::Weak<AppWindow>, intents: Intents, settling: Settling) {
    std::thread::spawn(move || loop {
        let started = Instant::now();
        // Sampled BEFORE the query goes out, so the comparison at paint time spans the whole
        // life of this report — not just the part after it came back.
        let epoch_at_query = intents.epoch();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(hostlink::send(KINDLE_QUERY, hostlink::QUICK_BUSY_WAIT));
        });

        // Stamped the moment the reply lands, not when it is painted — the closure below can
        // run seconds later, and `Settling` needs to know when this report was *true*.
        let mut captured = Instant::now();
        let report = match rx.recv_timeout(HEARTBEAT_TIMEOUT) {
            Ok(Ok(st)) => {
                captured = Instant::now();
                Some(st)
            }
            Ok(Err(_)) => None, // couldn't reach the host: offline
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Answer the user now, then block until the stuck request settles so the
                // next tick starts from a clean single-request state.
                if panel_gone(weak.upgrade_in_event_loop(|ui| apply_offline(&ui))) {
                    return;
                }
                let _ = rx.recv();
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };

        // Two checks, both read INSIDE the closure, on the UI thread, at the instant of
        // painting — not out here. There are seconds between a report arriving and its
        // closure running, and a Play started inside that gap would be repainted away by a
        // reading captured before it existed: the switch visibly flips back, which is the
        // exact bounce this check exists to stop.
        //
        // `busy()` alone is not enough, because it can be true, then false again, entirely
        // within that gap: this thread can be descheduled between receiving its reply above
        // and posting the closure below, and a whole Play can queue, run and drain in there.
        // The closure would then wake to `pending == 0` and faithfully paint a report from
        // before the click. Comparing the epoch against the value sampled *before the query
        // went out* closes it: any command at all, still running or long finished, makes this
        // report too old to draw the transport from.
        let intents = intents.clone();
        let settle = settling.clone();
        let posted = weak.upgrade_in_event_loop(move |ui| {
            let busy = intents.busy() || intents.epoch() != epoch_at_query;
            match report {
                // A command of ours is in flight (or the host says it's mid-command): take
                // the health, leave the transport alone. The settling check still runs — a
                // flip cannot have landed while the host is mid-command, so this holds the
                // lock rather than releasing it early.
                Some(st) if busy || st.busy => {
                    ui.set_host_online(true);
                    ui.set_kindle_running(st.kindle_running);
                    ui.set_speaking(ui.get_previewing() || st.kindle_speaking);
                    ui.set_read_aloud_locked(settle.observe(&st, captured));
                }
                Some(st) => {
                    apply_report(&ui, &st);
                    ui.set_read_aloud_locked(settle.observe(&st, captured));
                }
                None if busy => ui.set_host_online(false),
                None => {
                    settle.clear();
                    apply_offline(&ui);
                    ui.set_read_aloud_locked(false);
                }
            }
        });
        if panel_gone(posted) {
            return;
        }

        // Pace from the start of the tick, so a slow reply shortens the wait rather than
        // adding to it. A tick that overran simply starts the next at once. While a flip is
        // settling the cadence tightens to `SETTLE_POLL`: the switch is locked until a report
        // says otherwise, so at 1 Hz the user would wait up to a second past the moment it
        // could have been released.
        let period = if settling.pending() { SETTLE_POLL } else { HEARTBEAT_PERIOD };
        let elapsed = started.elapsed();
        if elapsed < period {
            std::thread::sleep(period - elapsed);
        }
    });
}

/// Whether a failed `upgrade_in_event_loop` means the panel is really gone.
///
/// Only `EventLoopTerminated` does. `NoEventLoopProvider` means the loop *hasn't started
/// yet* — and the heartbeat is deliberately started before `ui.run()`, so its first tick can
/// legitimately land in that window. Treating the two alike would end the loop on its first
/// post and leave the panel reading "offline" for the rest of the session, with no recovery
/// short of reopening it: precisely the stuck-forever health display this all exists to
/// prevent. Anything else is transient by assumption — keep beating and try again next tick.
fn panel_gone(posted: Result<(), slint::EventLoopError>) -> bool {
    matches!(posted, Err(slint::EventLoopError::EventLoopTerminated))
}
