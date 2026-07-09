//! Watches for `Kindle.exe` and injects the Kokoro `SetVoice` hook when it appears.
//!
//! Kindle 18632's narrator forces the WinRT default voice via `ISpVoice::SetVoice`, ignoring
//! our SAPI `DefaultTokenId` (see `ARCHITECTURE.md` / memory
//! `kindle-18632-spvoiceengine-regression`). The only fix is to run code inside Kindle that
//! redirects `SetVoice` to the Kokoro token. This host is x64 and Kindle is x86, so the actual
//! injection is done by the separate x86 `kokoro-inject.exe`, which `LoadLibrary`-loads
//! `kokoro_hook.dll` — this module only *detects* Kindle and *spawns* that helper.
//!
//! Isolated here so the synth/pipe code carries no injection concern. Nothing panics — any
//! failure logs and returns, never disturbing the audio path. Gated on the `kindle_kokoro`
//! `controls.json` flag; edge-triggered per Kindle PID so a running Kindle isn't re-injected
//! but a restart (new PID) is.

use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

const TARGET: &str = "Kindle.exe";

/// PID of the first process named `name`, if running. x64 enumeration sees WOW64 (x86)
/// processes by name/PID fine — only x86 *module* enumeration from x64 fails.
fn find_pid(name: &str) -> Option<u32> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut pe = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let end = pe
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(pe.szExeFile.len());
                if String::from_utf16_lossy(&pe.szExeFile[..end]).eq_ignore_ascii_case(name) {
                    found = Some(pe.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Resolve a bundled x86 artifact: `resources\<file>` next to the host exe (installed
/// layout), else the sibling crate's x86 release build (dev). Mirrors `main::panel_exe_path`.
fn resource_path(file: &str, _dev_crate: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("resources").join(file);
            if p.exists() {
                return p;
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(_dev_crate)
            .join("target")
            .join("i686-pc-windows-msvc")
            .join("release")
            .join(file);
        if dev.exists() {
            return dev;
        }
    }
    PathBuf::from(file)
}

fn injector_exe_path() -> PathBuf {
    resource_path("kokoro-inject.exe", "kokoro-inject")
}

fn hook_dll_path() -> PathBuf {
    resource_path("kokoro_hook.dll", "kokoro-hook")
}

/// Whether auto-injection is enabled — the panel's `kindle_kokoro` flag (default `true`, to
/// match the panel default). Read live so a panel toggle lands on the next Kindle launch.
fn enabled(app_data: &Path) -> bool {
    match std::fs::read_to_string(app_data.join("controls.json")) {
        // trim a leading UTF-8 BOM: the panel writes none, but a hand-edit (e.g. Notepad)
        // can add one, and serde_json rejects it — this flag gates an invasive action, so
        // an explicit `false` must be honored rather than falling through to the default.
        Ok(txt) => serde_json::from_str::<serde_json::Value>(txt.trim_start_matches('\u{feff}'))
            .ok()
            .and_then(|v| v.get("kindle_kokoro").and_then(|x| x.as_bool()))
            .unwrap_or(true),
        Err(_) => true, // no controls.json yet -> default on
    }
}

/// One watcher tick. Inject into a newly-seen Kindle when enabled; edge-triggered by PID so a
/// still-running Kindle isn't re-injected, but a restart (new PID) is. Never panics.
pub fn tick(app_data: &Path, last_injected_pid: &mut Option<u32>) {
    let Some(pid) = find_pid(TARGET) else {
        *last_injected_pid = None; // Kindle gone -> a new instance should re-inject
        return;
    };
    if *last_injected_pid == Some(pid) {
        return; // already handled this Kindle instance
    }
    if !enabled(app_data) {
        return; // disabled -> leave Kindle alone (re-checked each tick)
    }
    let injector = injector_exe_path();
    let hook = hook_dll_path();
    match Command::new(&injector).arg(&hook).spawn() {
        Ok(_) => {
            *last_injected_pid = Some(pid);
            eprintln!("[host] kindle-watch: spawned injector for Kindle pid {pid}");
        }
        Err(e) => eprintln!(
            "[host] kindle-watch: failed to spawn {}: {e}",
            injector.display()
        ),
    }
}
