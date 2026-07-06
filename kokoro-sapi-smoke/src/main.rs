//! No-Kindle, no-elevation smoke test for the Rust SAPI DLL (`kokoro-sapi-rs`).
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
use windows::Win32::Media::Speech::{SPVA_Speak, SPVSTATE, SPVTEXTFRAG};
use windows::Win32::System::Com::IClassFactory;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_core::{implement, interface, s, Interface, GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR};

// Must match kokoro-sapi-rs.
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
// Write). IID + slot order match kokoro-sapi-rs::sapi::ISpTTSEngineSite.
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

#[implement(ISpTTSEngineSite)]
struct TestSite {
    pcm: Arc<Mutex<Vec<u8>>>, // bytes the engine writes (int16 mono @ 24 kHz)
}

impl ISpTTSEngineSite_Impl for TestSite_Impl {
    unsafe fn AddEvents(&self, _e: *const c_void, _c: u32) -> HRESULT {
        S_OK
    }
    unsafe fn GetEventInterest(&self, i: *mut u64) -> HRESULT {
        if !i.is_null() {
            *i = 0;
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

/// Drive the engine's real `Speak` path against a running kokoro-host: build a text
/// fragment + a capturing site, call Speak, and confirm PCM came back. Reports SKIP if
/// the host isn't serving the pipe (Speak returns E_FAIL with no audio).
unsafe fn speak_test(engine: &ISpTTSEngine) {
    let pcm = Arc::new(Mutex::new(Vec::<u8>::new()));
    let site: ISpTTSEngineSite = TestSite { pcm: pcm.clone() }.into();

    let text: Vec<u16> = "This is a Kokoro speech test.".encode_utf16().collect();
    let mut state: SPVSTATE = core::mem::zeroed();
    state.eAction = SPVA_Speak;
    let frag = SPVTEXTFRAG {
        pNext: null_mut(),
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
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        r"..\kokoro-sapi-rs\target\i686-pc-windows-msvc\release\KokoroSapi.dll".into()
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
        speak_test(&engine);
    }
    finish();
}

fn finish() {
    let (pass, fail) = (PASS.load(Ordering::Relaxed), FAIL.load(Ordering::Relaxed));
    println!("\n{pass} passed, {fail} failed");
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
