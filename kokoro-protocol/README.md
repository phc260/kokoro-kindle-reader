# kokoro-protocol — the pipe wire format (shared crate)

The single source of truth for the named-pipe wire format between `kokoro-host` (x64) and
the clients that connect to it: the pipe name, the commands, the
`STREAM_END`/`SYNTH_ERROR`/`CHUNK_INFO` sentinels (`0xFFFF_FFFE` / `0xFFFF_FFFF` /
`0xFFFF_FFFD`), and the sample rate.

| Command | Who sends it | What it does |
|---|---|---|
| `'S'` `CMD_SYNTH` | `kokoro-sapi` (in Kindle), the panel's Preview | Synthesize an utterance; streams PCM frames back, **paced to ~real time**. |
| `'T'` `CMD_STATUS` | the panel | Milliseconds since the host last wrote audio to *any* client — "is Kokoro speaking right now?". Answered inline, not on the synth worker. |
| `'B'` `CMD_BENCH` | the panel | Time one execution provider on a fixed host-owned sample, **unpaced**, so the panel's speed test can say which of GPU/CPU is faster on this machine. One at a time host-wide: a second request gets `BENCH_BUSY` rather than being queued, since a measurement holds the synth worker for tens of seconds and anything on the machine can open the pipe. |

The `'S'` stream being paced is why `'B'` has to exist. Pacing is right for Kindle — the SAPI
engine plays what it's handed as it's handed it — and wrong for every caller that isn't a
real-time sink: timing a `'S'` request measures the pacing, so any engine faster than real time
clocks in at ~1.0x. `'B'` is the unpaced path that can answer the question honestly.

**Every client of this pipe is a real-time sink**, which is what keeps the command set this
small. The browser extension is *not* one — it schedules onto its own AudioContext cursor and
depends on synthesis outrunning playback — and it does not come through here at all: it reaches
`kokoro-host` over loopback HTTP (`kokoro-host/src/webserve.rs`), which calls
`native_synth::synth` directly. There was briefly an unpaced pipe command (`'R'`,
`CMD_SYNTH_RAW`) and a JSON info command (`'I'`) serving a native-messaging bridge; the bridge
was retired and both went with it. If a non-real-time pipe client ever appears, the three
differences it will need are on record in that commit: unpaced, no gain (a client running
seconds ahead would freeze a stale volume into unheard audio), and the voice named on the wire.

Pure constants, no deps — it builds for **both** architectures. It exists as its own crate
(one `src/lib.rs`) so both ends link the same definitions and **can't drift**; if the host
and the DLL ever disagreed on the format, Kindle would get silence or garbage.

`kokoro-host`, `kokoro-sapi` and `kokoro-panel` all depend on it by `path`. Don't inline these
constants in any consumer — change them here.
