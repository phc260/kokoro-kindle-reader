# kokoro-inject — the x86 LoadLibrary injector

A minimal x86 exe that `kokoro-host`'s Kindle-watcher spawns (fire-and-forget) whenever
it sees `Kindle.exe` start: it `LoadLibrary`-loads `kokoro_hook.dll` (see
[`../kokoro-hook`](../kokoro-hook/README.md)) into Kindle, which then patches
`ISpVoice::SetVoice` so Kindle narrates with Kokoro.

```
kokoro-inject.exe <path\to\kokoro_hook.dll>
```

## Build

```powershell
# One-time: the 32-bit Rust target (Kindle is a 32-bit process).
rustup target add i686-pc-windows-msvc

cargo build --release --target i686-pc-windows-msvc
# -> target\i686-pc-windows-msvc\release\kokoro-inject.exe
```

## Layout

| File | What |
|---|---|
| `src/main.rs` | `find_pid("Kindle.exe")` (ToolHelp), then `inject()`: `OpenProcess` + `VirtualAllocEx` + `WriteProcessMemory` + `CreateRemoteThread(LoadLibraryW)`. Windowless in release (`windows_subsystem = "windows"` — it's spawned by the windowless host, so there's no console to write to); outcomes go to `%TEMP%\kokoro-inject.log` instead. Nothing panics into a Windows error dialog — every failure path returns `Err` and exits with a nonzero code. |

## Gotchas

- **Must stay x86 and same-bitness as the target.** The injector reuses its own
  `kernel32!LoadLibraryW` address in Kindle's address space — that only works because
  `kernel32.dll` loads at the same address in every WOW64 (x86-on-x64) process for the
  boot. An x64 injector's `LoadLibraryW` address would be meaningless in 32-bit Kindle.
- **Needs matching integrity level.** Injection needs the host and Kindle at the same
  integrity (both normal user); if Kindle ever runs elevated, `OpenProcess` fails —
  logged and skipped, not fatal.
- **`kokoro-host` (x64) spawns this, never injects itself** — see
  `kokoro-host/src/kindle_watch.rs`. This exe does the one privileged-looking syscall
  sequence so the host binary itself doesn't have to.
- The dev fallback path (`../kokoro-hook/target/i686-pc-windows-msvc/release/kokoro_hook.dll`
  when no argv is given) is for manual testing only — the host always passes an explicit
  staged path.

See the repo-root [`CLAUDE.md`](../CLAUDE.md) / [`ARCHITECTURE.md`](../ARCHITECTURE.md)
for how this fits the Kindle-18632 hook mechanism end to end.
