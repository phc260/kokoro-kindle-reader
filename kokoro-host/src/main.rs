// Synthesis host for Kokoro Kindle Reader — the SAPI pipe + native Dawn WebGPU
// synth. A tray icon (tao message loop) is the only GUI; the settings panel is a
// separate process. The tokio pipe server runs on a background thread.
//
//   - native_synth.rs (serialized Rust WebGPU synth worker + controls.json reader)
//   - text.rs / espeak.rs (the kokoro-js text normalizer + espeak-ng FFI)
//   - split_text.rs   (the sentence-chunk splitter)
//   - model_patch.rs  (the in-memory ONNX graph edit exposing per-token durations)
// are plain modules here. The ORT/espeak runtime DLLs + espeak-ng-data are staged by
// build.rs.

// Windows GUI subsystem: no console window when launched from Explorer / at login.
// (Under `cargo run` a console is still attached by the parent.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

mod ctx;
mod espeak;
mod kindle_ctl;
mod kindle_state;
mod kindle_watch;
#[path = "../../legal.rs"]
mod legal;
mod model_patch;
mod native_synth;
mod split_text;
mod state;
mod text;

mod pipe;
mod webserve;

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

/// Spawn the native synth worker, the tokio pipe server, and the loopback HTTP endpoint,
/// all on one background thread. The tray/event loop stays on the main thread.
///
/// Two client paths, ONE serialized synth worker: Kindle over the pipe, and the browser
/// extension over this HTTP endpoint. They queue behind each other rather than contending,
/// which is the only correct arrangement — espeak has global state and the ORT session is
/// owned by that one worker.
///
/// HTTP is the browser's ONLY transport; see webserve.rs for why, and why not two.
///
/// Returns the shared [`kindle_state::KindleState`] so the tray's Kindle-watcher can publish
/// into it too — it already looks for Kindle every tick, so the panel-facing "is Kindle
/// running?" flag costs nothing to keep fresh.
fn start_pipe_server() -> std::sync::Arc<kindle_state::KindleState> {
    let app_data = app_data_dir();
    let base = app_data.join(MODEL_ID);
    let espeak = espeak_data_dir();

    eprintln!("[host] app_data = {}", app_data.display());
    eprintln!("[host] model base = {}", base.display());
    if !base.join("onnx").join("model.onnx").exists() {
        eprintln!("[host] WARNING: model.onnx not found — synthesis fails until the model is downloaded.");
    }

    // BEFORE anything that could build a session. The synth worker and kokoro-ocr's worker both
    // use ORT and would otherwise race to decide which library the process loads — and they do
    // not decide it the same way (see `init_ort`). Doing it here means the answer is settled
    // before either thread exists, and neither has to care about the other.
    if let Err(e) = native_synth::init_ort(&native_synth::exe_dir()) {
        eprintln!("[host] {e}");
    }

    let native = native_synth::NativeSynth::spawn(base.clone(), espeak);
    // Built ONCE, then shared by both transports. Cloning a `CoreCtx` shares the worker, the
    // general audio clock and the bench slot rather than copying them — two of any of those
    // is the bug this shape exists to make hard to write.
    let core = ctx::CoreCtx {
        app_data: app_data.clone(),
        // Where the voices/*.bin live, for the HTTP endpoint's /status voice list.
        model_base: base,
        native,
        // The general cell: when audio last went out to anyone, and whether a bench holds
        // the one worker.
        state: std::sync::Arc::new(state::HostState::default()),
    };
    // What the host believes Kindle is doing, over that same general state. Only the pipe
    // path and the watcher can reach it; the HTTP endpoint below gets `core` and nothing else.
    let kindle_state = kindle_state::KindleState::new(core.state.clone());
    // The Kindle-control thread — the only place in the project that touches Kindle's UI.
    // Blocking UI Automation lives on its own OS thread, off this tokio runtime and off the
    // serialized synth worker, so a heartbeat or a Stop can't queue behind a Play.
    let kindle = kindle_ctl::KindleCtl::spawn(kindle_state.clone());
    let pipe_ctx = pipe::KindleCtx {
        core: core.clone(),
        state: kindle_state.clone(),
        kindle,
    };

    // The web endpoint is best-effort: a failure to create or bind it must not take the pipe
    // down with it, because Kindle depends on the pipe and not on this.
    let web = match webserve::Endpoint::load_or_create(&app_data) {
        Ok(ep) => Some(webserve::WebCtx::new(core, std::sync::Arc::new(ep), &app_data)),
        Err(e) => {
            eprintln!("[host] web endpoint disabled: {e}");
            None
        }
    };

    std::thread::Builder::new()
        .name("kokoro-pipe".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime");
            rt.block_on(async move {
                if let Some(web) = web {
                    // A taken port (a second host instance, or the sibling kokoro-web-host)
                    // ends this task alone; the pipe below keeps serving Kindle.
                    tokio::spawn(async move {
                        if let Err(e) = webserve::serve_loop(web).await {
                            eprintln!("[host] web endpoint stopped: {e}");
                        }
                    });
                }
                if let Err(e) = pipe::serve_loop(pipe_ctx).await {
                    eprintln!("[host] pipe server stopped: {e}");
                }
            });
        })
        .expect("spawn pipe thread");

    kindle_state
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
    let kindle_state = start_pipe_server();
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
    let settings_i = MenuItem::new("Settings", true, None);
    // The browser extension needs a port + token pasted into its options page once per browser.
    // A release host is a windows-subsystem exe with no console to print them to, so the only
    // way out is the file — this opens it. That makes the item load-bearing whenever the
    // browser path ships: without it the transport is unusable, because there is nowhere else
    // the token is visible.
    //
    // It was off through 0.3.3, where offering a pairing code for a transport that release
    // didn't ship would have handed the user a dead end whose only honest explanation is
    // "ignore this". v0.4.x IS the browser path, and no browser change can be tested against
    // a real host until the port and token are visible again, so it goes back on first.
    // Delete the constant once that path actually ships to users; a permanently-true flag is
    // noise, and the reasoning it guards is preserved above it.
    const SHOW_WEB_PAIRING: bool = true;
    let pairing_i = MenuItem::new("Web pairing code", true, None);
    let legal_i = MenuItem::new("About && licenses", true, None);
    let quit_i = MenuItem::new("Quit", true, None);
    menu.append(&settings_i).expect("append settings");
    if SHOW_WEB_PAIRING {
        menu.append(&pairing_i).expect("append pairing");
    }
    menu.append(&legal_i).expect("append legal notices");
    menu.append(&tray_icon::menu::PredefinedMenuItem::separator())
        .expect("append separator");
    menu.append(&quit_i).expect("append quit");
    let settings_id = settings_i.id().clone();
    let pairing_id = pairing_i.id().clone();
    let legal_id = legal_i.id().clone();
    let quit_id = quit_i.id().clone();
    // Track the panel child so a second Settings click doesn't pile up windows.
    let mut panel_child: Option<std::process::Child> = None;

    // Kindle-watcher state: the event loop wakes on a timer and injects the hook once per
    // Kindle instance (edge-triggered by PID), retrying while the injector reports failure.
    // See kindle_watch.rs.
    let app_data = app_data_dir();
    let mut kindle = kindle_watch::Watch::default();
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
                kindle_watch::tick(&app_data, &mut kindle, &kindle_state);
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
                } else if menu_event.id == pairing_id {
                    // Hand it to the shell's default handler for .json rather than assuming an
                    // editor is installed. `explorer` also avoids the console window a `cmd /c
                    // start` would flash from a windows-subsystem process.
                    let path = webserve::endpoint_path(&app_data_dir());
                    if let Err(e) = std::process::Command::new("explorer").arg(&path).spawn() {
                        eprintln!("[host] failed to open {}: {e}", path.display());
                    }
                } else if menu_event.id == legal_id {
                    if let Err(e) = legal::open() {
                        eprintln!("[host] failed to open legal notices: {e}");
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
