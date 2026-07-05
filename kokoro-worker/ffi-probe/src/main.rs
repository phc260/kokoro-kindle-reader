// Drives the KokoroSynth WebGPU core through the C ABI (kokoro_ffi.h) from Rust,
// proving the Phase-2 FFI seam: create -> synth one chunk -> WAV. This mirrors what
// pipe_server.rs's spawn_synth will do instead of emit("synth-request").
//
// Usage: ffi-probe <model.onnx> <voice.bin> <tokenizer.json> <espeak-data> <out.wav> [text]

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr;

#[repr(C)]
struct KokoroWorker {
    _private: [u8; 0],
}

extern "C" {
    fn kokoro_worker_create(
        model: *const u16,
        voice: *const u16,
        tokenizer: *const u16,
        espeak_data: *const c_char,
        errbuf: *mut c_char,
        errcap: c_int,
    ) -> *mut KokoroWorker;
    fn kokoro_worker_synth(
        w: *mut KokoroWorker,
        text: *const c_char,
        speed: f32,
        out_pcm: *mut *mut f32,
        errbuf: *mut c_char,
        errcap: c_int,
    ) -> i64;
    fn kokoro_worker_free(pcm: *mut f32);
    fn kokoro_worker_destroy(w: *mut KokoroWorker);
}

fn u16z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn err_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&b| b != 0).map(|&b| b as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn write_wav(path: &str, pcm: &[f32], sr: u32) {
    let mut v: Vec<u8> = Vec::with_capacity(44 + pcm.len() * 2);
    let data_bytes = (pcm.len() * 2) as u32;
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&sr.to_le_bytes());
    v.extend_from_slice(&(sr * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in pcm {
        let c = s.clamp(-1.0, 1.0);
        let i = if c < 0.0 { (c * 32768.0) as i16 } else { (c * 32767.0) as i16 };
        v.extend_from_slice(&i.to_le_bytes());
    }
    std::fs::write(path, v).expect("write wav");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("usage: ffi-probe model voice tokenizer espeak-data out.wav [text]");
        std::process::exit(2);
    }
    let (model, voice, tok, espeak, out) = (&a[1], &a[2], &a[3], &a[4], &a[5]);
    let text = a.get(6).map(|s| s.as_str()).unwrap_or(
        "The quick brown fox jumps over the lazy dog. She sells sea shells by the sea shore.",
    );

    let (mw, vw, tw) = (u16z(model), u16z(voice), u16z(tok));
    let espeak_c = CString::new(espeak.as_str()).unwrap();
    let mut err = [0 as c_char; 512];

    let t0 = std::time::Instant::now();
    let w = unsafe {
        kokoro_worker_create(
            mw.as_ptr(),
            vw.as_ptr(),
            tw.as_ptr(),
            espeak_c.as_ptr(),
            err.as_mut_ptr(),
            err.len() as c_int,
        )
    };
    if w.is_null() {
        eprintln!("create failed: {}", err_string(&err));
        std::process::exit(1);
    }
    println!("init in {:.2} s", t0.elapsed().as_secs_f64());

    let text_c = CString::new(text).unwrap();
    let mut pcm_ptr: *mut f32 = ptr::null_mut();
    let t1 = std::time::Instant::now();
    let n = unsafe {
        kokoro_worker_synth(
            w,
            text_c.as_ptr(),
            1.0,
            &mut pcm_ptr,
            err.as_mut_ptr(),
            err.len() as c_int,
        )
    };
    if n < 0 {
        eprintln!("synth failed: {}", err_string(&err));
        unsafe { kokoro_worker_destroy(w) };
        std::process::exit(1);
    }
    let wall = t1.elapsed().as_secs_f64();
    let pcm: &[f32] = if pcm_ptr.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pcm_ptr, n as usize) }
    };
    let secs = pcm.len() as f64 / 24_000.0;
    write_wav(out, pcm, 24_000);
    println!(
        "synth: {} samples = {:.2} s audio in {:.2} s ({:.2}x realtime) -> {}",
        n, secs, wall, secs / wall, out
    );

    unsafe {
        kokoro_worker_free(pcm_ptr);
        kokoro_worker_destroy(w);
    }
}
