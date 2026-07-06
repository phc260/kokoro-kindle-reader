//! In-process COM server for the Kokoro SAPI5 voice — the x86 engine Kindle loads.
//! Connect-only: it forwards each `Speak` over the named pipe to the running
//! kokoro-host, which synthesizes and returns PCM. MUST be built x86 (Kindle is a
//! 32-bit process and loads this in-process).

// COM interface/method names and the KokoroSapi.dll crate name follow Windows
// conventions, not Rust's.
#![allow(non_snake_case)]

mod engine;
mod sapi;
mod worker;

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, ERROR_SUCCESS, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::StringFromGUID2;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_core::{GUID, HRESULT, PCWSTR};

use engine::Factory;
use worker::Worker;

// {0898F9AB-42C8-4DA5-A54F-520C9DD13C49} — the engine's CLSID.
const CLSID_KOKORO: GUID = GUID::from_u128(0x0898F9AB_42C8_4DA5_A54F_520C9DD13C49);

const FRIENDLY_NAME: &str = "Kokoro (SAPI5)";
const TOKEN_KEY: &str = r"SOFTWARE\Microsoft\Speech\Voices\Tokens\KokoroTTS";

// HRESULTs not in the windows crate.
const SELFREG_E_CLASS: HRESULT = HRESULT(0x8004_0201u32 as i32);
const SELFREG_E_TYPELIB: HRESULT = HRESULT(0x8004_0200u32 as i32);

// Process-global pipe client + synth serialization (the host handles one request at
// a time per connection). Mirrors the C++ `g_worker` / `g_synthMutex`.
pub(crate) static WORKER: Worker = Worker::new();
pub(crate) static SYNTH_LOCK: Mutex<()> = Mutex::new(());

// This module's HINSTANCE, captured in DllMain — needed to resolve the DLL's own path
// for registration.
static HINST: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

// ---- exported entry points ------------------------------------------------

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    iid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if !ppv.is_null() {
        *ppv = null_mut();
    }
    if clsid.is_null() || *clsid != CLSID_KOKORO {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactoryWrap = Factory.into();
    factory.0.query(iid, ppv)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    // Keep the DLL resident for the host process's lifetime (always safe).
    S_FALSE
}

/// Register the CLSID as an in-proc server and create the SAPI voice token. Writes to
/// HKLM — under the 32-bit regsvr32, WOW64 redirects to WOW6432Node, where 32-bit
/// hosts like Kindle read them.
#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    let dll_path = module_path();
    let clsid = guid_string(&CLSID_KOKORO);

    let clsid_key = format!(r"SOFTWARE\Classes\CLSID\{clsid}");
    if !set_string(&clsid_key, None, FRIENDLY_NAME) {
        return SELFREG_E_CLASS;
    }
    let inproc = format!(r"{clsid_key}\InprocServer32");
    if !set_string(&inproc, None, &dll_path) || !set_string(&inproc, Some("ThreadingModel"), "Both")
    {
        return SELFREG_E_CLASS;
    }

    if !set_string(TOKEN_KEY, None, FRIENDLY_NAME) || !set_string(TOKEN_KEY, Some("CLSID"), &clsid) {
        return SELFREG_E_TYPELIB;
    }
    let attrs = format!(r"{TOKEN_KEY}\Attributes");
    set_string(&attrs, Some("Name"), FRIENDLY_NAME);
    set_string(&attrs, Some("Vendor"), "kokoro-kindle-reader");
    set_string(&attrs, Some("Age"), "Adult");
    set_string(&attrs, Some("Gender"), "Female");
    set_string(&attrs, Some("Language"), "409"); // en-US
    set_string(&attrs, Some("VoiceFile"), "af_heart"); // informational only
    S_OK
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    let clsid = guid_string(&CLSID_KOKORO);
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(wide(&format!(r"SOFTWARE\Classes\CLSID\{clsid}")).as_ptr()));
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(wide(TOKEN_KEY).as_ptr()));
    }
    S_OK
}

#[no_mangle]
pub extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        HINST.store(hinst.0, Ordering::Release);
    }
    1 // TRUE
}

// ---- registry helpers -----------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Create (or open) `subkey` under HKLM and set one REG_SZ value (`name == None` for
/// the key's default value). Returns true on success.
fn set_string(subkey: &str, name: Option<&str>, value: &str) -> bool {
    let subkey_w = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let value_w = wide(value);
    let bytes = unsafe {
        core::slice::from_raw_parts(value_w.as_ptr() as *const u8, value_w.len() * 2)
    };
    let name_w = name.map(wide);
    let name_ptr = name_w.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    let rc = unsafe { RegSetValueExW(hkey, name_ptr, None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    rc == ERROR_SUCCESS
}

/// The `{XXXXXXXX-....}` registry form of a GUID.
fn guid_string(guid: &GUID) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { StringFromGUID2(guid, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(1) as usize - 1])
}

/// This DLL's own path (for InprocServer32).
fn module_path() -> String {
    let hmod = HMODULE(HINST.load(Ordering::Acquire));
    let mut buf = [0u16; 260];
    let n = unsafe { GetModuleFileNameW(Some(hmod), &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

// The `IClassFactory` produced by `#[implement]` is an interface pointer; wrap it so
// `DllGetClassObject` can QI it to the requested riid.
use windows::Win32::System::Com::IClassFactory;
use windows_core::Interface;
struct IClassFactoryWrap(IClassFactory);
impl From<Factory> for IClassFactoryWrap {
    fn from(f: Factory) -> Self {
        IClassFactoryWrap(f.into())
    }
}
