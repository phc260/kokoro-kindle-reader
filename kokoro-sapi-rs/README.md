# kokoro-sapi-rs — the x86 SAPI engine (Rust)

The in-process **x86 COM DLL** Kindle loads to narrate with Kokoro (`KokoroSapi.dll`).
It's **connect-only**: an `ISpTTSEngine` that forwards each `Speak` over
`\\.\pipe\KokoroSapiSynth` to the running `kokoro-host`, which synthesizes and streams
PCM back. It does no synthesis itself, so **`kokoro-host` must be running for Kindle to
speak**. This is the shipping engine — bundled and registered by the installer
(`packaging/build-installer.ps1` → `voice-setup.ps1`).

It must stay **x86** because Kindle is a 32-bit process that loads the DLL in-process by
registry path. A Rust panic can never unwind into Kindle — the crate builds with
`panic = "abort"`.

## Build

```powershell
# One-time: the 32-bit Rust target (Kindle is a 32-bit process).
rustup target add i686-pc-windows-msvc

cargo build --release --target i686-pc-windows-msvc
# -> target\i686-pc-windows-msvc\release\KokoroSapi.dll
```

No `.def` file is needed: `#[no_mangle] extern "system"` exports the four COM entry
points undecorated (`DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`,
`DllUnregisterServer`) — the names regsvr32 / COM look up — even on x86 stdcall.

## Layout

| File | What |
|---|---|
| `lib.rs` | The four COM exports + `DllMain`, the class factory, and registration (writes the CLSID `InprocServer32` + the `KokoroTTS` voice token). |
| `engine.rs` | `KokoroEngine` — `ISpTTSEngine` + `ISpObjectWithToken`; a pure streaming sink that forwards `Speak` over the pipe and writes ~250 ms PCM blocks back with `SPVES_ABORT` checks. |
| `worker.rs` | The pipe **client** (connect-only, no spawn). |
| `sapi.rs` | The `sapiddk.h` interfaces (`ISpTTSEngine`, `ISpTTSEngineSite`, `ISpObjectWithToken`), hand-declared via `#[interface]` because `windows-rs` ships only the SAPI *SDK* surface. |

The wire format lives in the shared **`kokoro-protocol`** crate, depended on by **both**
this DLL and `kokoro-host` — one source of truth, so the two ends can't drift. The SDK
structs/constants (`SPVTEXTFRAG`, `SPVA_*`, `SPVES_*`, `WAVEFORMATEX`) come from
`windows-rs`.

## Testing

Verified end-to-end in **Kindle for PC** (Read Aloud against a running `kokoro-host`),
and by `../kokoro-sapi-smoke` — a no-Kindle / no-registration / no-elevation harness:

- `DllGetClassObject` returns the class factory for the CLSID; `CreateInstance` produces
  the engine.
- QueryInterface across `ISpTTSEngine` / `ISpObjectWithToken` / `IUnknown` all succeed;
  a bogus IID returns `E_NOINTERFACE` — so `#[implement]` wires the multi-interface
  vtables correctly.
- `GetOutputFormat` **dispatches through the vtable** and returns 24 kHz/16-bit/mono,
  proving the hand-declared `ISpTTSEngine` slot order / IID are right.
- `DllCanUnloadNow` returns `S_FALSE`.
- With no host, `Speak` returns `E_FAIL` and no audio (the correct "no pipe, no
  fallback" behavior).

```powershell
cargo build -p kokoro-sapi-rs   --release --target i686-pc-windows-msvc
cargo run  -p kokoro-sapi-smoke --release --target i686-pc-windows-msvc
```

The smoke harness also has a `Speak`-path test: with a running host it supplies a fake
`ISpTTSEngineSite` that captures the PCM the engine writes through the real pipe. One
command builds, launches/reuses a host, runs it, and tears down what it started; add
`-Wav` to dump the engine's output for an audio check:

```powershell
.\kokoro-sapi-smoke\run-speak-test.ps1
.\kokoro-sapi-smoke\run-speak-test.ps1 -Wav engine.wav   # -> a 24 kHz mono WAV
```

## Dev registration (a real Kindle test — elevated, modifies the system)

The installer does this in production. To test a **local** build against Kindle, with
`kokoro-host` running:

```powershell
# Register the build (ELEVATED, 32-bit regsvr32 — from a STABLE path, not a temp dir).
C:\Windows\SysWOW64\regsvr32.exe "…\kokoro-sapi-rs\target\i686-pc-windows-msvc\release\KokoroSapi.dll"

# Drive it without Kindle (32-bit PowerShell, host running):
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File .\kokoro-sapi-rs\test-speak.ps1

# Then the real test: Read Aloud in Kindle. Set Kindle's default voice to Kokoro via the
# panel's Kindle-voice toggle (or kindle-voice-guard.ps1 -Set kokoro).
```

Register from a **stable path**, never a git worktree — the token's `InprocServer32`
stores the absolute DLL path it was registered from; if that path goes away, Kindle's
`LoadLibrary` fails silently and Read Aloud plays **nothing**. `regsvr32 /u` to unregister.
