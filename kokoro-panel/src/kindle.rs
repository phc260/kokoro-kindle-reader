// Switch Kindle's default SAPI voice between Kokoro and Microsoft David by running
// kindle-voice-guard.ps1 one-shot (-Set kokoro|david). The guard reg-loads Kindle's
// MSIX hive, which needs admin, so we relaunch it elevated via Start-Process -Verb
// RunAs (raises a UAC prompt).
// Blocking (waits on the UAC'd child) — run on a background thread.

use std::path::PathBuf;
use std::process::Command;

/// Locate kindle-voice-guard.ps1: next to the exe (bundle: resources/ or alongside),
/// else the dev checkout's kokoro-sapi-rs/.
fn guard_script_path() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [
                dir.join("resources").join("kindle-voice-guard.ps1"),
                dir.join("kindle-voice-guard.ps1"),
            ] {
                if cand.exists() {
                    return Ok(cand);
                }
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("kokoro-sapi-rs")
            .join("kindle-voice-guard.ps1");
        if dev.exists() {
            return Ok(dev);
        }
    }
    Err("kindle-voice-guard.ps1 not found next to the app.".to_string())
}

/// Set Kindle's default voice. `kokoro` true → Kokoro, false → Microsoft David.
/// Raises one UAC prompt; returns Err if the user cancels or the guard fails.
pub fn set_voice(kokoro: bool) -> Result<(), String> {
    let which = if kokoro { "kokoro" } else { "david" };
    let script = guard_script_path()?;
    // -Verb RunAs raises UAC; -Wait -PassThru lets us read the elevated guard's exit
    // code. A cancelled UAC throws -> catch -> exit 1. The path is single-quoted so
    // spaces survive as one ArgumentList element.
    let inner = format!(
        "$ErrorActionPreference='Stop'; try {{ $p = Start-Process -Verb RunAs \
         -FilePath powershell.exe -PassThru -Wait -ArgumentList \
         '-NoProfile','-ExecutionPolicy','Bypass','-File','{}','-Set','{}'; \
         exit $p.ExitCode }} catch {{ exit 1 }}",
        script.display(),
        which
    );
    let status = Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &inner])
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Kindle voice switch was cancelled or failed.".to_string());
    }
    Ok(())
}
