# kokoro-sapi-rs — Rust port of the x86 SAPI shim (PROTOTYPE)

A Rust reimplementation of `kokoro-sapi/` (the in-process x86 COM DLL Kindle loads to
narrate with Kokoro). This is a **prototype for A/B evaluation** against the C++ DLL —
it is not yet wired into the installer and has **not been verified inside Kindle**.

Same job as the C++ shim: a connect-only `ISpTTSEngine` that forwards each `Speak`
over `\\.\pipe\KokoroSapiSynth` to the running `kokoro-host`, which synthesizes and
streams PCM back. Same CLSID, same voice token, same wire protocol.

## Build

```powershell
# One-time: the 32-bit Rust target (Kindle is a 32-bit process).
rustup target add i686-pc-windows-msvc

cargo build --release --target i686-pc-windows-msvc
# -> target\i686-pc-windows-msvc\release\KokoroSapi.dll
```

No `.def` file is needed: `#[no_mangle] extern "system"` exports the four COM entry
points undecorated (`DllGetClassObject`, `DllCanUnloadNow`, `DllRegisterServer`,
`DllUnregisterServer`) — the names regsvr32 / COM look up — even on x86 stdcall. (The
C++ build needs `kokoro_sapi.def` for exactly this.)

## How it maps to the C++

| C++ | Rust |
|---|---|
| `Dll.cpp` (class factory, exports, registration, DllMain) | `lib.rs` + `engine.rs` (`Factory`) |
| `KokoroTTSEngine.cpp` (`ISpTTSEngine` + `ISpObjectWithToken`) | `engine.rs` (`KokoroEngine`) |
| `WorkerClient.cpp` (pipe client) | `worker.rs` |
| `WorkerProtocol.h` (wire format) | `protocol.rs` |
| `Guids.h` (CLSID) + `sapiddk.h` interfaces | `sapi.rs` (hand-declared via `#[interface]`) |

The `sapiddk.h` interfaces (`ISpTTSEngine`, `ISpTTSEngineSite`, `ISpObjectWithToken`)
are hand-declared because `windows-rs` only ships the SAPI *SDK* surface; the SDK
structs/constants (`SPVTEXTFRAG`, `SPVA_*`, `SPVES_*`, `WAVEFORMATEX`) come from the
crate. A Rust panic can never unwind into Kindle — the crate builds with
`panic = "abort"`.

## Status

**Verified statically:**
- Builds clean for `i686-pc-windows-msvc`, zero warnings.
- Output is a 32-bit PE32 DLL (~90 KB; the C++ is ~21 KB — both negligible vs the
  430 MB model).
- The four COM entry points are exported undecorated.

**Verified at runtime by `../kokoro-sapi-smoke` (no Kindle, no host, no elevation):**
- `DllGetClassObject` returns the class factory for the CLSID; `CreateInstance`
  produces the engine.
- QueryInterface across `ISpTTSEngine` / `ISpObjectWithToken` / `IUnknown` all
  succeed; a bogus IID returns `E_NOINTERFACE`. So `#[implement]` wires the
  multi-interface vtables correctly.
- `GetOutputFormat` **dispatches through the vtable** and returns 24 kHz/16-bit/mono —
  proving the hand-declared `ISpTTSEngine` slot order/IID are right.
- `DllCanUnloadNow` returns `S_FALSE`.

```powershell
cargo build -p kokoro-sapi-rs --release --target i686-pc-windows-msvc
cargo run  -p kokoro-sapi-smoke --release --target i686-pc-windows-msvc
```

**Still NOT verified — needs the real A/B test (see below):**
- The `Speak` path end-to-end: pipe streaming to `kokoro-host`, audio parity with the
  C++ engine, abort/stop (close-to-cancel), and volume/rate response.
- Behavior once loaded inside Kindle specifically.

## A/B test (manual, your call — modifies the system)

The Rust DLL uses the **same CLSID** as the C++ one, so only one can be registered at a
time. With `kokoro-host` running:

```powershell
# 1. Unregister the C++ DLL if registered, then register the Rust build (ELEVATED,
#    32-bit regsvr32 — from a STABLE path, not a temp dir).
C:\Windows\SysWOW64\regsvr32.exe "…\kokoro-sapi-rs\target\i686-pc-windows-msvc\release\KokoroSapi.dll"

# 2. Drive it without Kindle (32-bit PowerShell, host running):
C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe -File ..\kokoro-sapi\test-speak.ps1

# 3. Then the real test: Read Aloud in Kindle.
# To revert: regsvr32 /u the Rust DLL and re-register the C++ one.
```

## If it graduates

Replace `kokoro-sapi/` and point `packaging/build-installer.ps1` +
`kokoro-sapi/voice-setup.ps1` at this DLL. The genuine upside: `protocol.rs` could be
promoted to a crate shared with `kokoro-host`, retiring the `WorkerProtocol.h` ⇆
`pipe.rs` "change it in both places" duplication.
