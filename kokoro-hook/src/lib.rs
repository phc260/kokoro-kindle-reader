//! The x86 hook DLL `kokoro-host` injects into Kindle to restore Kokoro narration on
//! Kindle-for-PC 1.0.18632.0+.
//!
//! Background (see `ARCHITECTURE.md` / memory `kindle-18632-spvoiceengine-regression`): the
//! 7/7 Kindle build's `SpVoiceEngine::useSystemVoice` resolves its narration voice from the
//! WinRT `SpeechSynthesizer` default (Microsoft Zira) and applies it via `ISpVoice::SetVoice`,
//! ignoring the classic SAPI5 `DefaultTokenId` our installer points at Kokoro. The engine is
//! still classic `ISpVoice`, so the whole thing hinges on which token reaches `SetVoice`.
//!
//! On load, this DLL patches the process-wide `ISpVoice` vtable slot for `SetVoice` (index 18)
//! so that *whatever* token Kindle asks for, the real `SetVoice` runs with the Kokoro token
//! instead. `SpVoice`'s vtable is shared by every instance in the process, so a single patch
//! covers the instance Kindle creates. Everything downstream (`Speak` -> load `KokoroSapi.dll`
//! -> pipe to `kokoro-host`) is then Kindle's own, unmodified path.
//!
//! The patch is in-memory only (no persistent change); it lasts until Kindle exits. `verify()`
//! and the `selftest` bin exercise the override without Kindle — `selftest` is the fast
//! regression check for the `SetVoice` vtable index (slot 18).

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::core::{Interface, HRESULT, PCWSTR};
use windows::Win32::Media::Speech::{ISpObjectToken, ISpVoice, SpObjectToken, SpVoice};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::System::Memory::{
    VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};

/// Full SAPI token id for the Kokoro classic voice (32-bit SAPI resolves the WOW6432Node
/// view for us). Matches `kokoro-sapi`'s registration.
const KOKORO_ID: PCWSTR =
    windows::core::w!("HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech\\Voices\\Tokens\\KokoroTTS");

/// Non-Kokoro tokens to stand in for the voice Kindle's engine tries to apply (Zira),
/// used only by the in-process self-verification. First that resolves wins.
const OTHER_IDS: &[PCWSTR] = &[
    windows::core::w!("HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech\\Voices\\Tokens\\TTS_MS_EN-US_DAVID_11.0"),
    windows::core::w!("HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech\\Voices\\Tokens\\TTS_MS_EN-US_ZIRA_11.0"),
];

/// Vtable index of `ISpVoice::SetVoice`.
/// IUnknown(0..2), ISpNotifySource(3..9), ISpEventSource(10..12),
/// ISpVoice: SetOutput(13) GetOutputObjectToken(14) GetOutputStream(15) Pause(16)
/// Resume(17) **SetVoice(18)** GetVoice(19) Speak(20) ...
const SETVOICE_SLOT: usize = 18;

type SetVoiceFn = unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT;

static ORIG_SETVOICE: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static KOKORO_TOKEN: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Replacement `SetVoice`: forward to the real one but force the Kokoro token.
unsafe extern "system" fn hook_setvoice(this: *mut c_void, _requested: *mut c_void) -> HRESULT {
    let orig = ORIG_SETVOICE.load(Ordering::Acquire);
    if orig.is_null() {
        return HRESULT(0); // S_OK; should never happen once installed
    }
    let orig: SetVoiceFn = core::mem::transmute(orig);
    orig(this, KOKORO_TOKEN.load(Ordering::Acquire))
}

/// Install the vtable patch. Idempotent. Returns a human-readable status for logging.
pub fn install() -> windows::core::Result<String> {
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return Ok("already installed".into());
    }
    unsafe {
        // COM may already be initialised in the target; ignore RPC_E_CHANGED_MODE / S_FALSE.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        // Resolve + leak the Kokoro token so its pointer stays valid for the process life.
        let token: ISpObjectToken = CoCreateInstance(&SpObjectToken, None, CLSCTX_ALL)?;
        token.SetId(PCWSTR::null(), KOKORO_ID, false)?;
        KOKORO_TOKEN.store(token.as_raw(), Ordering::Release);
        core::mem::forget(token);

        // Create an SpVoice purely to reach the shared class vtable; leak it so sapi.dll
        // (and therefore this vtable) stays resident even before Kindle makes its own.
        let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL)?;
        let vtbl: *mut *const c_void = *(voice.as_raw() as *mut *mut *const c_void);
        let slot = vtbl.add(SETVOICE_SLOT);

        ORIG_SETVOICE.store(*slot as *mut c_void, Ordering::Release);

        let n = core::mem::size_of::<*const c_void>();
        let mut old = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(slot as *const c_void, n, PAGE_EXECUTE_READWRITE, &mut old)?;
        *slot = hook_setvoice as *const c_void;
        let mut restored = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(slot as *const c_void, n, old, &mut restored);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(slot as *const c_void), n);

        core::mem::forget(voice);
        Ok(format!(
            "installed: patched ISpVoice::SetVoice (slot {SETVOICE_SLOT}); orig=0x{:x}",
            ORIG_SETVOICE.load(Ordering::Acquire) as usize
        ))
    }
}

/// Prove the patch is live *in this process* (the real Kindle when injected): resolve a
/// non-Kokoro token, `SetVoice(other)`, then `GetVoice` must report Kokoro. If the flag
/// file `%TEMP%\kokoro-hook-speak` exists, also drive a real synchronous `Speak` — this
/// loads `KokoroSapi.dll` in-process and streams from `kokoro-host`, exercising the whole
/// chain end-to-end (audio + host synth).
pub fn verify() -> String {
    unsafe {
        let voice: ISpVoice = match CoCreateInstance(&SpVoice, None, CLSCTX_ALL) {
            Ok(v) => v,
            Err(e) => return format!("verify: SpVoice create failed: {e:?}"),
        };
        // Resolve a stand-in for Kindle's Zira.
        let mut other: Option<ISpObjectToken> = None;
        for id in OTHER_IDS {
            if let Ok(t) = CoCreateInstance::<_, ISpObjectToken>(&SpObjectToken, None, CLSCTX_ALL) {
                if t.SetId(PCWSTR::null(), *id, false).is_ok() {
                    other = Some(t);
                    break;
                }
            }
        }
        let Some(other) = other else {
            return "verify: no standard MS voice to test against".into();
        };
        if let Err(e) = voice.SetVoice(&other) {
            return format!("verify: SetVoice(other) failed: {e:?}");
        }
        let got = match voice.GetVoice() {
            Ok(t) => {
                let s = t
                    .GetId()
                    .ok()
                    .map(|p| {
                        let s = p.to_string().unwrap_or_default();
                        CoTaskMemFree(Some(p.as_ptr() as *const _));
                        s
                    })
                    .unwrap_or_default();
                s
            }
            Err(e) => return format!("verify: GetVoice failed: {e:?}"),
        };
        let overridden = got.to_ascii_lowercase().contains("kokorotts");
        let mut out = format!(
            "verify: SetVoice(other) -> GetVoice='{got}' -> override {}",
            if overridden { "OK (Kokoro)" } else { "FAILED" }
        );

        // Optional real Speak (audio + host synth), gated by a flag file.
        let speak = std::env::var("TEMP")
            .map(|d| std::path::Path::new(&format!("{d}\\kokoro-hook-speak")).exists())
            .unwrap_or(false);
        if speak {
            let text = windows::core::w!("Kokoro hook live test. One, two, three.");
            match voice.Speak(text, 0, None) {
                Ok(()) => out.push_str("; Speak OK (streamed from host)"),
                Err(e) => out.push_str(&format!("; Speak FAILED: {e:?} (is kokoro-host running?)")),
            }
        }
        out
    }
}

// ---- DLL entry point ------------------------------------------------------

fn log(msg: &str) {
    use std::io::Write;
    if let Ok(dir) = std::env::var("TEMP") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}\\kokoro-hook.log"))
        {
            let _ = writeln!(f, "[kokoro-hook] {msg}");
        }
    }
}

#[no_mangle]
extern "system" fn DllMain(_hinst: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        // Install off the loader lock (COM must not run inside DllMain).
        std::thread::spawn(|| match install() {
            Ok(s) => log(&s),
            Err(e) => log(&format!("install FAILED: {e:?}")),
        });
    }
    1
}
