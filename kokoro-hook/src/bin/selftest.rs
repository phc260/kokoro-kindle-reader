//! Kindle-free proof that the vtable override works. No injection, no audio.
//!
//! It reproduces exactly what Kindle's `SpVoiceEngine` does at the COM level:
//! `CoCreateInstance(SpVoice)` then `ISpVoice::SetVoice(<some token>)`. We:
//!   1. pick a real non-Kokoro voice token ("other"),
//!   2. SetVoice(other) BEFORE the hook and confirm GetVoice() == other (normal SAPI),
//!   3. install the hook,
//!   4. SetVoice(other) AGAIN and confirm GetVoice() == Kokoro (override works).
//!
//! If step 4 reports Kokoro, injecting `kokoro_hook.dll` into Kindle applies the identical
//! patch to the identical shared vtable — so Kindle's SetVoice(Zira) becomes Kokoro too.
//!
//!   cargo run --release --target i686-pc-windows-msvc --bin selftest

#![allow(non_snake_case)]

use windows::core::PCWSTR;
use windows::Win32::Media::Speech::{ISpObjectToken, ISpVoice, SpObjectToken, SpVoice};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
};

const KOKORO_ID: &str = r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices\Tokens\KokoroTTS";

// Candidate non-Kokoro voices to drive SetVoice with; first one that resolves wins.
const OTHER_CANDIDATES: &[&str] = &[
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices\Tokens\TTS_MS_EN-US_DAVID_11.0",
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices\Tokens\TTS_MS_EN-US_ZIRA_11.0",
    r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech_OneCore\Voices\Tokens\MSTTS_V110_enUS_ZiraM",
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn token_by_id(id: &str) -> windows::core::Result<ISpObjectToken> {
    let t: ISpObjectToken = CoCreateInstance(&SpObjectToken, None, CLSCTX_ALL)?;
    let w = wide(id);
    t.SetId(PCWSTR::null(), PCWSTR(w.as_ptr()), false)?;
    Ok(t)
}

unsafe fn id_of(t: &ISpObjectToken) -> String {
    match t.GetId() {
        Ok(p) => {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.as_ptr() as *const _));
            s
        }
        Err(e) => format!("<GetId failed: {e:?}>"),
    }
}

fn is_kokoro(id: &str) -> bool {
    id.to_ascii_lowercase().contains("kokorotts")
}

fn main() {
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut check = |name: &str, ok: bool| {
        if ok {
            pass += 1;
            println!("  PASS  {name}");
        } else {
            fail += 1;
            println!("  FAIL  {name}");
        }
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        // Kokoro must be registered for any of this to make sense.
        let kok = token_by_id(KOKORO_ID);
        check("Kokoro token resolves (registered)", kok.is_ok());
        if kok.is_err() {
            println!("\n  Kokoro not registered - run the installer / regsvr32 first.");
            std::process::exit(1);
        }

        // Pick a real non-Kokoro voice to stand in for Kindle's Zira.
        let mut other: Option<ISpObjectToken> = None;
        for id in OTHER_CANDIDATES {
            if let Ok(t) = token_by_id(id) {
                println!("  using 'other' voice: {}", id_of(&t));
                other = Some(t);
                break;
            }
        }
        let other = match other {
            Some(t) => t,
            None => {
                check("found a non-Kokoro voice to test with", false);
                println!("\n  No standard MS voice found; can't run the before/after.");
                std::process::exit(1);
            }
        };

        let voice: ISpVoice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL).expect("SpVoice");

        // (2) Baseline: normal SAPI honours SetVoice.
        voice.SetVoice(&other).expect("SetVoice(other)");
        let before = id_of(&voice.GetVoice().expect("GetVoice"));
        println!("  before hook: GetVoice -> {before}");
        check("baseline: SetVoice(other) is honoured (not Kokoro)", !is_kokoro(&before));

        // (3) Install the hook (same code the injected DLL runs).
        match kokoro_hook::install() {
            Ok(s) => println!("  {s}"),
            Err(e) => {
                check("hook install", false);
                println!("  install error: {e:?}");
            }
        }

        // (4) Same call Kindle makes; must now come back Kokoro.
        voice.SetVoice(&other).expect("SetVoice(other) after hook");
        let after = id_of(&voice.GetVoice().expect("GetVoice after hook"));
        println!("  after hook:  GetVoice -> {after}");
        check("override: SetVoice(other) now yields Kokoro", is_kokoro(&after));
    }

    println!("\n{pass} passed, {fail} failed");
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
