// Engine speed test: ask the host to time the GPU and the CPU execution provider on
// its own fixed sample, so the panel can answer "which should I pick?" with a measured
// number instead of leaving the user to guess at a checkbox.
//
// The timing deliberately does NOT go through Preview/CMD_SYNTH: that stream is paced
// to ~real time (pipe.rs's 500 ms lead), so anything faster than realtime would clock in
// at ~1.0x and the two engines would look identical. CMD_BENCH runs the model unpaced on
// the host's synth worker and reports how much speech it rendered per second of wall
// clock. Blocking, and slow (tens of seconds per engine) — run it on a background thread.

use std::io::{Read, Write};

use kokoro_protocol::{
    BENCH_BUSY, BENCH_ENGINE_CPU, BENCH_ENGINE_GPU, BENCH_FAILED, BENCH_OK, CMD_BENCH,
};

/// One engine's measured throughput.
#[derive(Clone, Copy)]
pub struct Speed {
    /// Seconds of speech rendered per second of wall clock. Above 1.0 means the machine
    /// synthesizes faster than the audio plays, i.e. it can keep up with reading.
    pub realtime: f32,
}

/// Time one execution provider. `Ok(None)` means the host ran the test and this engine
/// isn't usable here (no working GPU adapter, model missing); `Err` means the test
/// couldn't be run at all (host unreachable, pipe error) and says so in the user's words.
pub fn measure(gpu: bool) -> Result<Option<Speed>, String> {
    let mut pipe = crate::preview::connect()?;
    let selector = if gpu { BENCH_ENGINE_GPU } else { BENCH_ENGINE_CPU };
    pipe.write_all(&[CMD_BENCH, selector]).map_err(|e| format!("pipe write: {e}"))?;
    pipe.flush().map_err(|e| format!("pipe flush: {e}"))?;

    // [u32 status][f32 audioSecs][f32 elapsedSecs]. The read blocks for the whole
    // measurement — the host answers only once it has finished timing.
    let mut buf = [0u8; 12];
    pipe.read_exact(&mut buf).map_err(|e| format!("pipe read: {e}"))?;
    let status = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let audio = f32::from_le_bytes(buf[4..8].try_into().unwrap());
    let elapsed = f32::from_le_bytes(buf[8..12].try_into().unwrap());

    if status == BENCH_FAILED {
        return Ok(None);
    }
    // Busy is not "this engine doesn't work" — the host refuses a second measurement
    // rather than queueing it, so say what actually happened instead of condemning the
    // engine. Reachable from a second panel instance, or anything else on the machine
    // that speaks the protocol.
    if status == BENCH_BUSY {
        return Err("another speed test is already running — wait for it to finish.".to_string());
    }
    if status != BENCH_OK {
        return Err("the host sent an unexpected test result.".to_string());
    }
    // A zero/negative span would divide to infinity or NaN; treat it as unusable rather
    // than reporting an absurd speed.
    if !(elapsed > 0.0) || !(audio > 0.0) {
        return Ok(None);
    }
    Ok(Some(Speed { realtime: audio / elapsed }))
}
