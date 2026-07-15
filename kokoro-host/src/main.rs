// Synthesis host for Kokoro Kindle Reader — the SAPI pipe + native Dawn WebGPU
// synth. A tray icon (tao message loop) is the only GUI; the settings panel is a
// separate process. The tokio pipe server runs on a background thread.
//
//   - native_synth.rs (serialized Rust WebGPU synth worker + controls.json reader)
//   - text.rs / espeak.rs (the kokoro-js text normalizer + espeak-ng FFI)
//   - split_text.rs   (the sentence-chunk splitter)
// are plain modules here. The ORT/espeak runtime DLLs + espeak-ng-data are staged by
// build.rs.

// Windows GUI subsystem: no console window when launched from Explorer / at login.
// (Under `cargo run` a console is still attached by the parent.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

mod espeak;
mod kindle_watch;
mod native_synth;
mod split_text;
mod text;

mod pipe;

use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};

// The app identifier — the model + controls.json live under %APPDATA%\<identifier>
// on Windows (matches prior releases, so an existing install's data is reused).
const APP_IDENTIFIER: &str = "com.phc260.kokoro-kindle-reader";
// The pinned model's repo id (from model-manifest.json); the model files
// live under <app_data>/<MODEL_ID>/. Embedded so we don't parse the manifest at
// runtime just for this one string.
const MODEL_ID: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
// HKCU Run value name — matches prior releases so login autostart isn't duplicated.
// Only read by the release-gated enable_autostart, hence allow(dead_code) in debug.
#[cfg_attr(debug_assertions, allow(dead_code))]
const AUTOSTART_NAME: &str = "kokoro-kindle-reader";

/// The app-data dir on Windows: %APPDATA% (Roaming) \ <identifier>.
fn app_data_dir() -> PathBuf {
    let roaming = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    roaming.join(APP_IDENTIFIER)
}

/// espeak-ng-data staged next to this exe by build.rs.
fn espeak_data_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("espeak-ng-data")
}

/// Spawn the native synth worker + tokio pipe server on a background thread. The
/// tray/event loop stays on the main thread.
fn start_pipe_server() {
    let app_data = app_data_dir();
    let base = app_data.join(MODEL_ID);
    let espeak = espeak_data_dir();

    eprintln!("[host] app_data = {}", app_data.display());
    eprintln!("[host] model base = {}", base.display());
    if !base.join("onnx").join("model.onnx").exists() {
        eprintln!("[host] WARNING: model.onnx not found — synthesis fails until the model is downloaded.");
    }

    let native = native_synth::NativeSynth::spawn(base, espeak);
    let ctx = pipe::Ctx {
        app_data,
        native,
        // Shared "last audio written" clock the pipe answers CMD_STATUS from.
        last_audio_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    std::thread::Builder::new()
        .name("kokoro-pipe".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            rt.block_on(async move {
                if let Err(e) = pipe::serve_loop(ctx).await {
                    eprintln!("[host] pipe server stopped: {e}");
                }
            });
        })
        .expect("spawn pipe thread");
}

/// Register the host to launch hidden at login (release only, so a dev run doesn't
/// hijack the installed app's Run entry).
#[cfg(not(debug_assertions))]
fn enable_autostart() {
    let Ok(exe) = std::env::current_exe() else { return };
    let built = auto_launch::AutoLaunchBuilder::new()
        .set_app_name(AUTOSTART_NAME)
        .set_app_path(&exe.to_string_lossy())
        .set_args(&["--hidden"])
        .build();
    match built {
        Ok(al) => {
            if let Err(e) = al.enable() {
                eprintln!("[host] autostart enable failed: {e}");
            }
        }
        Err(e) => eprintln!("[host] autostart build failed: {e}"),
    }
}

/// Locate the settings panel exe: next to the host exe (bundle layout), else the
/// sibling crate's dev build.
fn panel_exe_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("kokoro-panel.exe");
            if p.exists() {
                return p;
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("kokoro-panel")
            .join("target")
            .join("debug")
            .join("kokoro-panel.exe");
        if dev.exists() {
            return dev;
        }
    }
    PathBuf::from("kokoro-panel.exe")
}

fn load_tray_icon() -> tray_icon::Icon {
    let bytes = include_bytes!("../../icons/32x32.png");
    let img = image::load_from_memory(bytes)
        .expect("decode tray icon")
        .to_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).expect("tray icon rgba")
}

fn main() {
    start_pipe_server();
    #[cfg(not(debug_assertions))]
    enable_autostart();

    // tao message loop hosts the tray. Menu clicks arrive via MenuEvent's global
    // channel; we forward them into the loop as user events so a Quit click wakes
    // a `Wait`-blocked loop deterministically (no polling / CPU spin).
    let event_loop = EventLoopBuilder::<MenuEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = proxy.send_event(event);
    }));

    let menu = Menu::new();
    let settings_i = MenuItem::new("Settings…", true, None);
    let quit_i = MenuItem::new("Quit", true, None);
    menu.append(&settings_i).expect("append settings");
    menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
        .expect("append separator");
    menu.append(&quit_i).expect("append quit");
    let settings_id = settings_i.id().clone();
    let quit_id = quit_i.id().clone();
    // Track the panel child so a second Settings click doesn't pile up windows.
    let mut panel_child: Option<std::process::Child> = None;

    // Kindle-watcher state: the event loop wakes on a timer and injects the hook once per
    // Kindle instance (edge-triggered by PID). See kindle_watch.rs.
    let app_data = app_data_dir();
    let mut last_injected_pid: Option<u32> = None;
    const KINDLE_POLL: Duration = Duration::from_secs(4);

    // Build the tray after the event loop exists (its message-only window needs the
    // loop's thread). Kept alive by moving into the run closure.
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Kokoro Kindle Reader")
        .with_icon(load_tray_icon())
        .build()
        .expect("build tray");

    eprintln!("[host] tray up; serving \\\\.\\pipe\\KokoroSapiSynth");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + KINDLE_POLL);
        // Keep the tray alive for the loop's lifetime.
        let _ = &tray;
        match event {
            // Timer wake (or first run): poll for Kindle and inject the hook if needed.
            Event::NewEvents(StartCause::ResumeTimeReached { .. } | StartCause::Init) => {
                kindle_watch::tick(&app_data, &mut last_injected_pid);
            }
            Event::UserEvent(menu_event) => {
                if menu_event.id == settings_id {
                    // Spawn the panel unless one is already open (try_wait -> None
                    // means still running).
                    let alive = panel_child
                        .as_mut()
                        .map(|c| matches!(c.try_wait(), Ok(None)))
                        .unwrap_or(false);
                    if !alive {
                        let path = panel_exe_path();
                        match std::process::Command::new(&path).spawn() {
                            Ok(child) => panel_child = Some(child),
                            Err(e) => {
                                eprintln!("[host] failed to launch panel {}: {e}", path.display())
                            }
                        }
                    }
                } else if menu_event.id == quit_id {
                    // The pipe thread is a daemon; exiting the process stops it and
                    // frees the pipe so Kindle's next Speak fails fast (page-done).
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
        // Drain tray-icon click events so they don't accumulate (unused for now).
        while TrayIconEvent::receiver().try_recv().is_ok() {}
    });
}
