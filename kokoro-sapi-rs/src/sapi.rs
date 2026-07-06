//! The three `sapiddk.h` COM interfaces the engine needs, hand-declared because
//! `windows-rs` only ships the SAPI *SDK* surface (`sapi.h`), not the DDK. Vtable
//! order and IIDs must match `sapiddk.h` exactly. We call `ISpTTSEngineSite`;
//! we implement `ISpTTSEngine` + `ISpObjectWithToken`.

use core::ffi::c_void;

use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::SPVTEXTFRAG;
use windows_core::{interface, GUID, HRESULT, IUnknown, IUnknown_Vtbl};

/// {C31ADBAE-527F-4FF5-A230-F62BB61FF70C} — declared PCM output format id.
pub const SPDFID_WAVEFORMATEX: GUID = GUID::from_u128(0xC31ADBAE_527F_4FF5_A230_F62BB61FF70C);

/// The site SAPI hands to `Speak`. Full vtable in declaration order (we only call
/// `GetActions` / `Write` / `GetRate` / `GetVolume`, but the earlier/later slots must
/// exist so offsets line up). Inherits `ISpEventSink` (AddEvents / GetEventInterest).
#[interface("9880499B-CCE9-11D2-B503-00C04F797396")]
pub unsafe trait ISpTTSEngineSite: IUnknown {
    pub unsafe fn AddEvents(&self, events: *const c_void, count: u32) -> HRESULT;
    pub unsafe fn GetEventInterest(&self, interest: *mut u64) -> HRESULT;
    pub unsafe fn GetActions(&self) -> u32; // SPVESACTIONS, returned by value
    pub unsafe fn Write(&self, buf: *const c_void, cb: u32, written: *mut u32) -> HRESULT;
    pub unsafe fn GetRate(&self, rate: *mut i32) -> HRESULT;
    pub unsafe fn GetVolume(&self, volume: *mut u16) -> HRESULT;
    pub unsafe fn GetSkipInfo(&self, ty: *mut i32, items: *mut i32) -> HRESULT;
    pub unsafe fn CompleteSkip(&self, skipped: i32) -> HRESULT;
}

/// The engine interface SAPI calls. We implement `Speak` + `GetOutputFormat`.
#[interface("A74D7C8E-4CC5-4F2F-A6EB-804DEE18500E")]
pub unsafe trait ISpTTSEngine: IUnknown {
    unsafe fn Speak(
        &self,
        flags: u32,
        fmt_id: *const GUID,
        wfx: *const WAVEFORMATEX,
        frags: *const SPVTEXTFRAG,
        site: *mut c_void,
    ) -> HRESULT;
    unsafe fn GetOutputFormat(
        &self,
        target_fmt_id: *const GUID,
        target_wfx: *const WAVEFORMATEX,
        out_fmt_id: *mut GUID,
        out_wfx: *mut *mut WAVEFORMATEX,
    ) -> HRESULT;
}

/// Lets SAPI hand us the voice token after creating the engine.
#[interface("5B559F40-E952-11D2-BB91-00C04F8EE6C0")]
pub unsafe trait ISpObjectWithToken: IUnknown {
    unsafe fn SetObjectToken(&self, token: *mut c_void) -> HRESULT;
    unsafe fn GetObjectToken(&self, token: *mut *mut c_void) -> HRESULT;
}
