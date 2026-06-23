#pragma once
// Wire protocol between clients (the 32-bit SAPI engine, the kokoro-reader
// Tauri backend) and the 64-bit synthesis worker over a byte-mode named pipe.
//
// Every request starts with a one-byte command:
//
//   kCmdSynth ('S'):
//     -> [float rate][u32 textBytes][utf8 text]
//     <- [u32 nSamples][float samples...]      (24 kHz mono, [-1, 1])
//        nSamples == kSynthError signals a synthesis failure.
//     `rate` is the host's rate-derived speed multiplier (1 = the host's normal
//     rate). The synthesis host (the kokoro-reader app) owns the narrator voice
//     and the user's speed multiplier — it reads those from its own settings
//     (webview localStorage) and folds `rate` into the final synthesis speed — so
//     they're no longer carried on the wire. Gain is NOT applied here; the engine
//     queries it at playback (kCmdGain) and scales the samples then.
//
//   kCmdGain ('G'):
//     -> (nothing)
//     <- [float gain]                          (1 = unity; the user's volume)
//     The engine asks for the current gain when each chunk *starts playing*, so a
//     volume change lands within the playing chunk instead of being frozen into
//     already-synthesized/prefetched samples. The host reads "tts-gain" from its
//     webview localStorage. Unity (1.0) is the safe fallback if it can't answer.
//
//   kCmdChunk ('C'):
//     -> (nothing)
//     <- [u32 sentences]                       (sentences per steady-state chunk)
//     The engine asks once per Speak how many sentences to coalesce per chunk
//     (after the always-1-sentence first chunk, which stays fixed for fast start).
//     The host reads "tts-chunk" from its webview localStorage; the engine clamps
//     it and keeps its built-in default if the query fails.
//
//   kCmdInfo ('I'):
//     -> (nothing)
//     <- [u16 jsonBytes][utf8 json]  e.g. {"provider":"DirectML","voice":"af_heart"}
//
// One sentence-sized chunk per synth request; clients handle chunking, abort and
// rate. The worker stays alive between requests (model stays warm) and exits
// after kIdleTimeoutMs without a client.
#include <windows.h>
#include <cstdint>

namespace kokoro_ipc {

constexpr wchar_t kPipeName[]     = L"\\\\.\\pipe\\KokoroSapiSynth";
constexpr uint8_t kCmdSynth       = 'S';
constexpr uint8_t kCmdGain        = 'G';
constexpr uint8_t kCmdChunk       = 'C';
constexpr uint8_t kCmdInfo        = 'I';
constexpr uint32_t kSynthError    = 0xFFFFFFFFu;
constexpr uint32_t kMaxTextBytes  = 1u << 20;   // sanity cap (1 MB)
constexpr uint32_t kIdleTimeoutMs = 5 * 60 * 1000;

// Byte-mode pipes may deliver partial reads/writes; both ends use these.
inline bool ReadExact(HANDLE pipe, void* buf, DWORD n) {
    auto* p = static_cast<char*>(buf);
    while (n) {
        DWORD got = 0;
        if (!ReadFile(pipe, p, n, &got, nullptr) || got == 0) return false;
        p += got;
        n -= got;
    }
    return true;
}

inline bool WriteExact(HANDLE pipe, const void* buf, DWORD n) {
    auto* p = static_cast<const char*>(buf);
    while (n) {
        DWORD put = 0;
        if (!WriteFile(pipe, p, n, &put, nullptr)) return false;
        p += put;
        n -= put;
    }
    return true;
}

}  // namespace kokoro_ipc
