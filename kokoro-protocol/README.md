# kokoro-protocol — the pipe wire format (shared crate)

The single source of truth for the named-pipe wire format between `kokoro-host` (x64) and
the `kokoro-sapi` SAPI DLL (x86): the pipe name, the `'S'`/`'I'` commands, the
`STREAM_END`/`SYNTH_ERROR` sentinels (`0xFFFF_FFFE` / `0xFFFF_FFFF`), and the sample rate.

Pure constants, no deps — it builds for **both** architectures. It exists as its own crate
(one `src/lib.rs`) so both ends link the same definitions and **can't drift**; if the host
and the DLL ever disagreed on the format, Kindle would get silence or garbage.

Both `kokoro-host` and `kokoro-sapi` depend on it by `path`. Don't inline these constants
in either consumer — change them here.
