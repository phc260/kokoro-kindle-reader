//! No-Kindle, no-elevation smoke test for the Rust SAPI DLL (`kokoro-sapi`).
//!
//! It never touches the registry: it `LoadLibrary`s the built DLL, calls its exported
//! `DllGetClassObject` to get the class factory, `CreateInstance`s the engine, and then
//! exercises the COM wiring that only a real host would otherwise hit:
//!   * QueryInterface across ISpTTSEngine / ISpObjectWithToken / IUnknown,
//!   * a bogus IID returns E_NOINTERFACE (not a crash / false success),
//!   * GetOutputFormat actually dispatches through the vtable and returns 24 kHz/16/mono.
//!
//! A wrong vtable slot or IID in the hand-declared interfaces shows up here as a failed
//! check instead of silence inside Kindle. Build/run 32-bit to match the DLL:
//!   cargo run --release --target i686-pc-windows-msvc -- <path-to>\KokoroSapi.dll

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr::null_mut;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{E_NOTIMPL, S_OK};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::{SPVA_Bookmark, SPVA_Speak, SPVSTATE, SPVTEXTFRAG};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_core::{implement, interface, s, Interface, GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR};

// Must match kokoro-sapi.
const CLSID_KOKORO: GUID = GUID::from_u128(0x0898F9AB_42C8_4DA5_A54F_520C9DD13C49);

// Minimal redeclarations so we can call across the vtable (only the slots we touch
// need real signatures; earlier slots keep the ABI aligned).
#[interface("A74D7C8E-4CC5-4F2F-A6EB-804DEE18500E")]
unsafe trait ISpTTSEngine: IUnknown {
    pub unsafe fn Speak(&self, a: u32, b: *const c_void, c: *const c_void, d: *const c_void, e: *mut c_void) -> HRESULT;
    pub unsafe fn GetOutputFormat(&self, a: *const GUID, b: *const WAVEFORMATEX, out_id: *mut GUID, out_wfx: *mut *mut WAVEFORMATEX) -> HRESULT;
}

#[interface("5B559F40-E952-11D2-BB91-00C04F8EE6C0")]
unsafe trait ISpObjectWithToken: IUnknown {
    pub unsafe fn SetObjectToken(&self, token: *mut c_void) -> HRESULT;
    pub unsafe fn GetObjectToken(&self, token: *mut *mut c_void) -> HRESULT;
}

// The site SAPI normally supplies to Speak; here a fake one that captures the PCM the
// engine writes. Full vtable in order (the engine calls GetActions/GetVolume/GetRate/
// Write). IID + slot order match kokoro-sapi::sapi::ISpTTSEngineSite.
#[interface("9880499B-CCE9-11D2-B503-00C04F797396")]
unsafe trait ISpTTSEngineSite: IUnknown {
    pub unsafe fn AddEvents(&self, events: *const c_void, count: u32) -> HRESULT;
    pub unsafe fn GetEventInterest(&self, interest: *mut u64) -> HRESULT;
    pub unsafe fn GetActions(&self) -> u32;
    pub unsafe fn Write(&self, buf: *const c_void, cb: u32, written: *mut u32) -> HRESULT;
    pub unsafe fn GetRate(&self, rate: *mut i32) -> HRESULT;
    pub unsafe fn GetVolume(&self, volume: *mut u16) -> HRESULT;
    pub unsafe fn GetSkipInfo(&self, ty: *mut i32, items: *mut i32) -> HRESULT;
    pub unsafe fn CompleteSkip(&self, skipped: i32) -> HRESULT;
}

// SPEVENT (x86): eEventId is the low 16 bits of the leading i32 bitfield.
const SPEI_TTS_BOOKMARK: u16 = 4;
const SPEI_WORD_BOUNDARY: u16 = 5;

#[implement(ISpTTSEngineSite)]
struct TestSite {
    pcm: Arc<Mutex<Vec<u8>>>,       // bytes the engine writes (int16 mono @ 24 kHz)
    events: Arc<Mutex<Vec<u16>>>,   // eEventIds the engine reports via AddEvents
}

impl ISpTTSEngineSite_Impl for TestSite_Impl {
    unsafe fn AddEvents(&self, e: *const c_void, c: u32) -> HRESULT {
        // Each SPEVENT is 24 bytes on x86; the first field is the eEventId|elParamType
        // bitfield, so the low 16 bits are the event id.
        if !e.is_null() {
            for k in 0..c as usize {
                let id = *(e as *const u8).add(k * 24).cast::<u16>();
                self.events.lock().unwrap().push(id);
            }
        }
        S_OK
    }
    unsafe fn GetEventInterest(&self, i: *mut u64) -> HRESULT {
        if !i.is_null() {
            *i = u64::MAX; // interested in every event (a real host sets specific bits)
        }
        S_OK
    }
    unsafe fn GetActions(&self) -> u32 {
        0 // no abort, no volume/rate change mid-stream
    }
    unsafe fn Write(&self, buf: *const c_void, cb: u32, written: *mut u32) -> HRESULT {
        if !buf.is_null() && cb > 0 {
            let slice = core::slice::from_raw_parts(buf as *const u8, cb as usize);
            self.pcm.lock().unwrap().extend_from_slice(slice);
        }
        if !written.is_null() {
            *written = cb;
        }
        S_OK
    }
    unsafe fn GetRate(&self, r: *mut i32) -> HRESULT {
        if !r.is_null() {
            *r = 0;
        }
        S_OK
    }
    unsafe fn GetVolume(&self, v: *mut u16) -> HRESULT {
        if !v.is_null() {
            *v = 100;
        }
        S_OK
    }
    unsafe fn GetSkipInfo(&self, _t: *mut i32, _n: *mut i32) -> HRESULT {
        E_NOTIMPL
    }
    unsafe fn CompleteSkip(&self, _n: i32) -> HRESULT {
        E_NOTIMPL
    }
}

/// Minimal 24 kHz / 16-bit / mono WAV writer. `pcm` is exactly the int16 LE bytes the
/// engine wrote, so we just prepend a 44-byte header.
fn write_wav(path: &str, pcm: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let (sr, ch, bits) = (24000u32, 1u16, 16u16);
    let block_align = ch * (bits / 8);
    let byte_rate = sr * block_align as u32;
    let data_len = pcm.len() as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // WAVE_FORMAT_PCM
    f.write_all(&ch.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm)?;
    Ok(())
}

/// Drive the engine's real `Speak` path against a running kokoro-host: build a text
/// fragment + a capturing site, call Speak, and confirm PCM came back (optionally
/// dumping it to `wav` for an audio A/B). Reports SKIP if the host isn't serving the
/// pipe (Speak returns E_FAIL with no audio).
unsafe fn speak_test(engine: &ISpTTSEngine, wav: Option<&str>) {
    let pcm = Arc::new(Mutex::new(Vec::<u8>::new()));
    let events = Arc::new(Mutex::new(Vec::<u16>::new()));
    let site: ISpTTSEngineSite = TestSite { pcm: pcm.clone(), events: events.clone() }.into();

    // A multi-word utterance followed by a bookmark fragment — exactly the shape Kindle
    // 18632's narrator sends (SSML text + <bookmark>). The engine must report word
    // boundaries and the bookmark back, or an event-driven narrator can't advance past
    // the first sentence.
    let text: Vec<u16> = "This is a Kokoro speech test.".encode_utf16().collect();
    let mark: Vec<u16> = "1".encode_utf16().collect();
    let mut mark_state: SPVSTATE = core::mem::zeroed();
    mark_state.eAction = SPVA_Bookmark;
    let bookmark = SPVTEXTFRAG {
        pNext: null_mut(),
        State: mark_state,
        pTextStart: PCWSTR(mark.as_ptr()),
        ulTextLen: mark.len() as u32,
        ulTextSrcOffset: text.len() as u32,
    };
    let mut state: SPVSTATE = core::mem::zeroed();
    state.eAction = SPVA_Speak;
    let frag = SPVTEXTFRAG {
        pNext: &bookmark as *const SPVTEXTFRAG as *mut SPVTEXTFRAG,
        State: state,
        pTextStart: PCWSTR(text.as_ptr()),
        ulTextLen: text.len() as u32,
        ulTextSrcOffset: 0,
    };

    println!("\nSpeak path (needs kokoro-host running):");
    let hr = engine.Speak(
        0,
        null_mut(),
        null_mut(),
        &frag as *const SPVTEXTFRAG as *const c_void,
        site.as_raw(),
    );
    let bytes = pcm.lock().unwrap().len();
    if hr.is_err() && bytes == 0 {
        println!("  SKIP  Speak returned {hr:?}, no audio — is kokoro-host running?");
        return;
    }
    let ms = bytes / 2 * 1000 / 24000; // int16 mono @ 24 kHz
    check(
        &format!("Speak streamed PCM ({bytes} bytes ≈ {ms} ms) hr={hr:?}"),
        hr.is_ok() && bytes > 0,
    );

    // The fix: the engine reports SAPI events so Kindle's narrator can advance/highlight.
    let ev = events.lock().unwrap();
    let words = ev.iter().filter(|&&e| e == SPEI_WORD_BOUNDARY).count();
    let bookmarks = ev.iter().filter(|&&e| e == SPEI_TTS_BOOKMARK).count();
    check(
        &format!("reported word-boundary events ({words} words)"),
        words >= 5, // "This is a Kokoro speech test." = 6 words
    );
    check(
        &format!("reported the bookmark event ({bookmarks})"),
        bookmarks == 1,
    );

    if let (Some(path), true) = (wav, bytes > 0) {
        match write_wav(path, &pcm.lock().unwrap()) {
            Ok(()) => println!("  wrote {path}"),
            Err(e) => println!("  WAV write failed: {e}"),
        }
    }
}

// A GUID we know the engine does NOT implement.
const IID_BOGUS: GUID = GUID::from_u128(0xDEADBEEF_0000_0000_0000_000000000001);

type DllGetClassObjectFn =
    unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut c_void) -> HRESULT;

use core::sync::atomic::{AtomicU32, Ordering};
static PASS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicU32 = AtomicU32::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        PASS.fetch_add(1, Ordering::Relaxed);
        println!("  PASS  {name}");
    } else {
        FAIL.fetch_add(1, Ordering::Relaxed);
        println!("  FAIL  {name}");
    }
}

fn main() {
    // Args: [<dll-path>] [--wav <out.wav>]. Both optional.
    let mut path: Option<String> = None;
    let mut wav: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--wav" => wav = args.next(),
            _ => {
                path.get_or_insert(a);
            }
        }
    }
    let path = path.unwrap_or_else(|| {
        r"..\kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll".into()
    });
    println!("Loading {path}");
    let wpath: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let module = LoadLibraryW(PCWSTR(wpath.as_ptr())).expect("LoadLibraryW failed");

        // DllCanUnloadNow -> S_FALSE (we keep it resident).
        if let Some(f) = GetProcAddress(module, s!("DllCanUnloadNow")) {
            let can: unsafe extern "system" fn() -> HRESULT = core::mem::transmute(f);
            check("DllCanUnloadNow returns S_FALSE", can().0 == 1);
        } else {
            check("DllCanUnloadNow exported", false);
        }

        // DllGetClassObject -> IClassFactory.
        let dgco = GetProcAddress(module, s!("DllGetClassObject")).expect("no DllGetClassObject");
        let dgco: DllGetClassObjectFn = core::mem::transmute(dgco);

        let mut factory_ptr: *mut c_void = null_mut();
        let hr = dgco(&CLSID_KOKORO, &IClassFactory::IID, &mut factory_ptr);
        check("DllGetClassObject(CLSID, IClassFactory) == S_OK", hr.is_ok() && !factory_ptr.is_null());
        if factory_ptr.is_null() {
            return finish();
        }
        let factory = IClassFactory::from_raw(factory_ptr);

        // CreateInstance -> ISpTTSEngine (QI'd internally to the custom interface).
        let engine: ISpTTSEngine = match factory.CreateInstance(None::<&IUnknown>) {
            Ok(e) => {
                check("CreateInstance(ISpTTSEngine) == S_OK", true);
                e
            }
            Err(_) => {
                check("CreateInstance(ISpTTSEngine) == S_OK", false);
                return finish();
            }
        };

        // QueryInterface across the object's interfaces.
        check("QI IUnknown", engine.cast::<IUnknown>().is_ok());
        check("QI ISpObjectWithToken", engine.cast::<ISpObjectWithToken>().is_ok());

        // A bogus IID must be refused, not crash / falsely succeed.
        let mut junk: *mut c_void = null_mut();
        let unk: IUnknown = engine.cast().unwrap();
        let hr = unk.query(&IID_BOGUS, &mut junk);
        check("QI bogus IID -> E_NOINTERFACE", hr.0 == windows::Win32::Foundation::E_NOINTERFACE.0 && junk.is_null());

        // GetOutputFormat actually dispatches through the vtable.
        let mut fmt_id = GUID::zeroed();
        let mut wfx: *mut WAVEFORMATEX = null_mut();
        let hr = engine.GetOutputFormat(null_mut(), null_mut(), &mut fmt_id, &mut wfx);
        let good_fmt = hr.is_ok()
            && !wfx.is_null()
            && (*wfx).nSamplesPerSec == 24000
            && (*wfx).wBitsPerSample == 16
            && (*wfx).nChannels == 1;
        check("GetOutputFormat -> 24kHz/16-bit/mono", good_fmt);
        if !wfx.is_null() {
            windows::Win32::System::Com::CoTaskMemFree(Some(wfx as *const c_void));
        }

        // The real synthesis path (requires a running host).
        speak_test(&engine, wav.as_deref());
    }
    finish();
}

fn finish() {
    let (pass, fail) = (PASS.load(Ordering::Relaxed), FAIL.load(Ordering::Relaxed));
    println!("\n{pass} passed, {fail} failed");
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
