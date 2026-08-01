# kokoro-protocol — the pipe wire format (shared crate)

The single source of truth for the named-pipe wire format between `kokoro-host` (x64) and
the clients that connect to it: the pipe name, the commands, the
`STREAM_END`/`SYNTH_ERROR`/`CHUNK_INFO` sentinels (`0xFFFF_FFFE` / `0xFFFF_FFFF` /
`0xFFFF_FFFD`), and the sample rate.

| Command | Who sends it | What it does |
|---|---|---|
| `'S'` `CMD_SYNTH` | `kokoro-sapi` (in Kindle) | Synthesize an utterance; streams PCM frames back, **paced to ~real time**. The only command that counts as *Kindle* audio. |
| `'P'` `CMD_PREVIEW` | the panel's Preview | `'S'`'s request and response exactly, **unpaced**, and off the Kindle-audio clock. |
| `'T'` `CMD_STATUS` | (legacy) | Milliseconds since the host last wrote audio to *any* client. Answered inline, not on the synth worker. |
| `'B'` `CMD_BENCH` | the panel | Time one execution provider on a fixed host-owned sample, **unpaced**, so the panel's speed test can say which of GPU/CPU is faster on this machine. One at a time host-wide: a second request gets `BENCH_BUSY` rather than being queued, since a measurement holds the synth worker for tens of seconds and anything on the machine can open the pipe. |
| `'K'` `CMD_KINDLE` | the panel | Kindle reading control (`QUERY` / `PLAY` / `STOP` / `PAUSE` / `RESUME` / `CLOSE`) and the host's state in reply. `QUERY` is the panel's ~1 Hz **heartbeat**. |

The `'S'` stream being paced is why `'B'` and `'P'` both exist. Pacing is right for Kindle —
the SAPI engine plays what it's handed as it's handed it — and wrong for every caller that
isn't a real-time sink: timing a `'S'` request measures the pacing, so any engine faster than
real time clocks in at ~1.0x, and a Preview that buffers the whole clip before playing it
would simply take as long to arrive as it takes to speak.

`'P'` earns its own byte for a second reason, and it's the more important one: without it the
host cannot tell its own panel's silent narrator prefetch from Kindle narrating a page, and
every consumer of "is Kokoro speaking?" has to guess from timing. It used to. `'K'`'s
`msSinceKindleAudio` is the answer that replaced the guess.

`'K'` is what makes `kokoro-host` the panel's **sole authority for Kindle reading and
health**. The panel sends intent and renders the reply; it does not open Kindle's window,
read its UI Automation tree, or send it keystrokes — the host's Kindle-control thread does,
and it's the only thing that does. Health is *reachability*: a pipe that won't open is an
offline host, which is a state a reply value can't express. `QUERY`, `PAUSE` and `RESUME` are
answered inline off atomics, so a heartbeat or a pause can never queue behind an in-flight
synthesis, a benchmark, or a `PLAY` still driving Kindle.

**Every client of this pipe that receives audio is a real-time sink except `'P'`**, which is
what keeps the command set this small. The browser extension is *not* one — it schedules onto
its own AudioContext cursor and
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
