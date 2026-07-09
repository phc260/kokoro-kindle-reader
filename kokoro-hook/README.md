# kokoro-hook — the x86 SetVoice-override DLL

The x86 cdylib `kokoro-host`'s watcher injects into `Kindle.exe` to restore Kokoro
narration on Kindle for PC **1.0.18632.0+**. That build's narrator (`SpVoiceEngine` in
`xrm120.dll`) resolves its voice from the WinRT `SpeechSynthesizer` default and applies
it via `ISpVoice::SetVoice`, ignoring the classic SAPI5 `DefaultTokenId` our installer
points at Kokoro. The engine is still classic `ISpVoice`, so the only lever is *which
token reaches `SetVoice`* — this DLL patches that.

On load it patches the process-shared `ISpVoice` vtable slot for `SetVoice`
(**index 18**) so any token Kindle requests is swapped for the Kokoro token before the
real `SetVoice` runs. `SpVoice`'s vtable is shared by every instance in the process, so
one patch covers the instance Kindle creates. Everything downstream (`Speak` → load
`KokoroSapi.dll` → pipe to `kokoro-host`) is then Kindle's own, unmodified path.

The patch is **in-memory only** — it lasts until Kindle exits; there's no
persistence/unhook. It never touches `KokoroSapi.dll`, which stays the untouched
connect-only SAPI engine (single responsibility).

## Build

```powershell
# One-time: the 32-bit Rust target (Kindle is a 32-bit process).
rustup target add i686-pc-windows-msvc

cargo build --release --target i686-pc-windows-msvc
# -> target\i686-pc-windows-msvc\release\kokoro_hook.dll
```

## Layout

| File | What |
|---|---|
| `src/lib.rs` | `install()` (the vtable patch, idempotent), `verify()` (in-process self-check + an optional real `Speak` gated by a `%TEMP%\kokoro-hook-speak` flag file), and `DllMain`, which spawns `install()` on a thread off the loader lock (COM must not run inside `DllMain`). Logs to `%TEMP%\kokoro-hook.log`. |
| `src/bin/selftest.rs` | Kindle-free proof the override works: `SetVoice(other)` before the hook (honoured, not Kokoro), install the hook, `SetVoice(other)` again (now yields Kokoro). No injection, no audio. |

## Testing

```powershell
# Fast regression check for the SetVoice vtable index (slot 18) — no Kindle, no audio.
# Needs the Kokoro voice registered (installer or dev regsvr32; see ../kokoro-sapi/README.md).
cargo run --release --target i686-pc-windows-msvc --bin selftest
```

Live-validated end-to-end inside real Kindle 1.0.18632.0: injected via `kokoro-inject`,
the hook logged `installed: patched ISpVoice::SetVoice (slot 18)`, then
`verify: SetVoice(other) -> GetVoice='...KokoroTTS' -> override OK`, then a real `Speak`
streamed from `kokoro-host` — confirmed by Kindle narrating in Kokoro's voice.

See the repo-root [`CLAUDE.md`](../CLAUDE.md) / [`ARCHITECTURE.md`](../ARCHITECTURE.md)
and memory `kindle-18632-spvoiceengine-regression` for the full root-cause writeup.

## Gotchas

- **Must stay x86** — Kindle is 32-bit; the DLL is injected in-process.
- **Slot 18 is the current SAPI ABI.** `selftest` guards it; a future Windows/SAPI change
  could shift the vtable layout.
- **`kokoro-host` (x64) never injects itself** — it only detects Kindle and spawns the
  separate x86 `kokoro-inject.exe`, which does the actual `LoadLibrary`.
