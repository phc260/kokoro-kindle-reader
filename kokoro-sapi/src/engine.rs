//! The SAPI5 voice engine object + its class factory. Mirrors `KokoroTTSEngine.cpp`:
//! a pure streaming sink that forwards the whole utterance to kokoro-host over the
//! pipe and pumps the returned PCM frames to the SAPI site.

use core::ffi::c_void;
use core::ptr::null_mut;
use std::sync::Mutex;

use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_OUTOFMEMORY, E_POINTER, S_OK};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::Speech::{
    SPVA_Pronounce, SPVA_Speak, SPVA_SpellOut, SPVTEXTFRAG, SPVES_ABORT, SPVES_VOLUME,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, IClassFactory, IClassFactory_Impl};
use windows_core::{implement, IUnknown, Interface, Ref, BOOL, GUID, HRESULT};

const WAVE_FORMAT_PCM: u16 = 1;

use crate::sapi::{
    ISpObjectWithToken, ISpObjectWithToken_Impl, ISpTTSEngine, ISpTTSEngineSite, ISpTTSEngine_Impl,
    SPDFID_WAVEFORMATEX,
};
use crate::worker::Frame;
use crate::{SYNTH_LOCK, WORKER};

const SAMPLE_RATE: u32 = 24000;

#[implement(ISpTTSEngine, ISpObjectWithToken)]
pub struct KokoroEngine {
    // The voice token SAPI hands us (held for its lifetime; never read).
    token: Mutex<Option<IUnknown>>,
}

impl KokoroEngine {
    pub fn new() -> Self {
        KokoroEngine { token: Mutex::new(None) }
    }
}

impl ISpObjectWithToken_Impl for KokoroEngine_Impl {
    unsafe fn SetObjectToken(&self, token: *mut c_void) -> HRESULT {
        let held = IUnknown::from_raw_borrowed(&token).cloned(); // clone = AddRef
        *self.token.lock().unwrap() = held;
        S_OK
    }

    unsafe fn GetObjectToken(&self, out: *mut *mut c_void) -> HRESULT {
        if out.is_null() {
            return E_POINTER;
        }
        match self.token.lock().unwrap().as_ref() {
            Some(tok) => {
                *out = tok.clone().into_raw(); // AddRef, transfer to caller
                S_OK
            }
            None => E_FAIL,
        }
    }
}

impl ISpTTSEngine_Impl for KokoroEngine_Impl {
    // Declare 24 kHz / 16-bit / mono PCM (Kokoro's native rate); SAPI inserts any
    // converter a host needs.
    unsafe fn GetOutputFormat(
        &self,
        _target_fmt_id: *const GUID,
        _target_wfx: *const WAVEFORMATEX,
        out_fmt_id: *mut GUID,
        out_wfx: *mut *mut WAVEFORMATEX,
    ) -> HRESULT {
        if out_fmt_id.is_null() || out_wfx.is_null() {
            return E_POINTER;
        }
        let wfx = CoTaskMemAlloc(core::mem::size_of::<WAVEFORMATEX>()) as *mut WAVEFORMATEX;
        if wfx.is_null() {
            return E_OUTOFMEMORY;
        }
        *wfx = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            wBitsPerSample: 16,
            nBlockAlign: 2,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            cbSize: 0,
        };
        *out_fmt_id = SPDFID_WAVEFORMATEX;
        *out_wfx = wfx;
        S_OK
    }

    unsafe fn Speak(
        &self,
        _flags: u32,
        _fmt_id: *const GUID,
        _wfx: *const WAVEFORMATEX,
        frags: *const SPVTEXTFRAG,
        site: *mut c_void,
    ) -> HRESULT {
        let Some(site) = ISpTTSEngineSite::from_raw_borrowed(&site) else {
            return E_POINTER;
        };

        // Connect to the host's synth pipe (silently skip if it isn't running).
        {
            let _lk = SYNTH_LOCK.lock().unwrap();
            if !WORKER.ensure_connected() {
                return E_FAIL;
            }
        }

        // Concatenate the speakable fragments (UTF-16 -> UTF-8).
        let mut utf16: Vec<u16> = Vec::new();
        let mut f = frags;
        while !f.is_null() {
            let frag = &*f;
            let a = frag.State.eAction;
            if (a == SPVA_Speak || a == SPVA_Pronounce || a == SPVA_SpellOut)
                && !frag.pTextStart.is_null()
                && frag.ulTextLen > 0
            {
                utf16.extend_from_slice(core::slice::from_raw_parts(
                    frag.pTextStart.0,
                    frag.ulTextLen as usize,
                ));
                utf16.push(0x20);
            }
            f = frag.pNext;
        }
        if utf16.is_empty() {
            return S_OK;
        }
        let text = String::from_utf16_lossy(&utf16);

        // Host SAPI rate -10..10 -> speed 1/3x..3x (log). The host folds in the user's
        // own speed. Rate is fixed for the utterance.
        let mut volume: u16 = 100;
        let _ = site.GetVolume(&mut volume);
        let mut rate: i32 = 0;
        let _ = site.GetRate(&mut rate);
        let speed = 3.0f32.powf(rate as f32 / 10.0);

        // Open the stream (one 'S' request), with a single reconnect retry.
        {
            let _lk = SYNTH_LOCK.lock().unwrap();
            if !WORKER.begin_synth(text.as_bytes(), speed)
                && !(WORKER.ensure_connected() && WORKER.begin_synth(text.as_bytes(), speed))
            {
                return E_FAIL;
            }
        }

        const BLOCK: usize = (SAMPLE_RATE / 4) as usize; // ~250 ms
        let mut result = S_OK;
        let mut aborted = false;
        loop {
            if site.GetActions() & SPVES_ABORT.0 as u32 != 0 {
                aborted = true;
                break;
            }
            let frame = {
                let _lk = SYNTH_LOCK.lock().unwrap();
                WORKER.read_frame()
            };
            let (pcm, gain) = match frame {
                Frame::End => break,
                Frame::Error => {
                    result = E_FAIL;
                    break;
                }
                Frame::Data { samples, gain } => (samples, gain),
            };

            // f32 [-1,1] -> i16 with the frame's gain x the host volume.
            if site.GetActions() & SPVES_VOLUME.0 as u32 != 0 {
                let _ = site.GetVolume(&mut volume);
            }
            let vol = volume as f32 / 100.0;
            let out: Vec<i16> = pcm
                .iter()
                .map(|&s| ((s * gain * vol).clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();

            for block in out.chunks(BLOCK) {
                if site.GetActions() & SPVES_ABORT.0 as u32 != 0 {
                    aborted = true;
                    break;
                }
                let mut written = 0u32;
                let hr = site.Write(
                    block.as_ptr() as *const c_void,
                    (block.len() * 2) as u32,
                    &mut written,
                );
                if hr.is_err() {
                    result = hr;
                    aborted = true;
                    break;
                }
            }
            if aborted {
                break;
            }
        }

        // Stopped early: close the pipe to interrupt the host's stream. Next Speak
        // reconnects. A clean End/Error leaves the pipe open for reuse.
        if aborted {
            WORKER.close();
        }
        result
    }
}

// ---- class factory --------------------------------------------------------

#[implement(IClassFactory)]
pub struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        object: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        if object.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *object = null_mut();
        }
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let engine: ISpTTSEngine = KokoroEngine::new().into();
        unsafe { engine.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}
