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

```powershell
cargo build -p kokoro-sapi      --release --target i686-pc-windows-msvc
cargo run  -p kokoro-sapi-smoke --release --target i686-pc-windows-msvc -- `
    ..\kokoro-sapi\target\i686-pc-windows-msvc\release\KokoroSapi.dll
```

## The Speak-path test (needs a running host)

`run-speak-test.ps1` builds everything, launches or reuses a `kokoro-host`, supplies a
fake `ISpTTSEngineSite` that captures the PCM the engine writes through the real pipe,
then tears down what it started. The Speak path self-**skips** if no host is available
(that's why CI still passes with no host on the runner).

```powershell
.\run-speak-test.ps1
.\run-speak-test.ps1 -Wav engine.wav   # -> a 24 kHz mono WAV for an audio check
```

See [`../kokoro-sapi/README.md`](../kokoro-sapi/README.md) for the engine under test.
