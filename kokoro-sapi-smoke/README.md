# kokoro-sapi-smoke — no-Kindle COM + Speak smoke test

A standalone harness that exercises the `kokoro-sapi` engine **without Kindle, without
registration, without elevation**: it `LoadLibrary`s the DLL and drives the COM object
directly, so a vtable/QI/slot-order regression fails loudly instead of silently breaking
Read Aloud. Run in CI on every `kokoro-sapi` change (`.github/workflows/sapi.yml`).

## The COM checks (no host needed)

`DllGetClassObject` → class factory → `CreateInstance`; QueryInterface across
`ISpTTSEngine` / `ISpObjectWithToken` / `IUnknown` (and `E_NOINTERFACE` for a bogus IID);
`GetOutputFormat` dispatched through the vtable returns 24 kHz/16-bit/mono;
`DllCanUnloadNow` returns `S_FALSE`. With no host, `Speak` returns `E_FAIL` (the correct
"no pipe, no fallback" behavior).

There is no root workspace, so `-p` cannot reach a sibling crate — each command names its
own manifest. From the **repo root** (the same two commands CI runs):

```powershell
cargo build --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi\Cargo.toml
cargo run   --release --target i686-pc-windows-msvc --manifest-path kokoro-sapi-smoke\Cargo.toml -- `
    kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll
```

Pass the DLL path: the built-in default (`..\kokoro-sapi\target\…`) is relative to the
**working directory**, not to the manifest, so it only resolves from inside this crate or
`kokoro-sapi\`.

## The Speak-path test (needs a running host)

`run-speak-test.ps1` builds everything, launches or reuses a `kokoro-host`, supplies a
fake `ISpTTSEngineSite` that captures the PCM the engine writes through the real pipe,
then tears down what it started. The Speak path self-**skips** if no host is available
(that's why CI still passes with no host on the runner).

The site captures each SPEVENT's **`ullAudioStreamOffset`**, not just its id, and the test
prints every word boundary with the moment it fires. Counting ids cannot distinguish a
correct event stream from one whose events all land at the wrong time — and the wrong time
is precisely what Kindle turns into a highlight on the wrong word. It also checks the
offsets are non-decreasing (SAPI requires it) and inside the audio.

The first word's offset is the quickest read on which timing path is live:

```
  'This'    350 ms      <- the graph patch took: the model's own onset
  'This'      0 ms      <- the host fell back to the stock graph, so the engine is
                           interpolating; a word at character zero is always 0 whatever
                           the leading silence
```

Both are correct outcomes, so this is reported as a NOTE rather than checked. The host's own
`session: … model-derived word timing` line says which happened and why.

```powershell
.\run-speak-test.ps1
.\run-speak-test.ps1 -Wav engine.wav   # -> a 24 kHz mono WAV for an audio check
```

See [`../kokoro-sapi/README.md`](../kokoro-sapi/README.md) for the engine under test.
