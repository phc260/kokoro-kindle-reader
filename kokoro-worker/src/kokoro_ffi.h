// C ABI around KokoroSynth so the Rust pipe host can drive native WebGPU synthesis
// (the Rust `ort` crate has no WebGPU EP, so synthesis stays C++). One opaque worker
// owns an ORT/WebGPU session + espeak + tokenizer + one voice. The host owns
// chunking/pacing and calls kokoro_worker_synth once per already-cut chunk.
//
// Paths are NUL-terminated UTF-16LE (uint16_t*) — matches std::wstring on Windows
// without a wchar_t ABI dependency. espeakDataDir is UTF-8. Error strings are copied
// into caller-provided buffers.
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct KokoroWorker KokoroWorker;

// Create + init a worker. Returns NULL on failure (errbuf gets the message).
KokoroWorker* kokoro_worker_create(const uint16_t* modelOnnx, const uint16_t* voiceBin,
                                   const uint16_t* tokenizerJson, const char* espeakDataDir,
                                   char* errbuf, int errcap);

// Synthesize `utf8Text` as ONE unit (no internal chunking) at `speed` (1 = normal).
// On success returns the f32 sample count (>= 0) and sets *outPcm to a malloc'd
// 24 kHz mono f32 buffer the caller frees with kokoro_worker_free. Returns -1 on
// failure (errbuf set). Empty/punctuation-only text returns 0 with *outPcm = NULL.
int64_t kokoro_worker_synth(KokoroWorker* w, const char* utf8Text, float speed,
                            float** outPcm, char* errbuf, int errcap);

// Switch narrator (reload voice.bin, keep the session). 0 = ok, -1 = fail (errbuf set).
int kokoro_worker_set_voice(KokoroWorker* w, const uint16_t* voiceBin, char* errbuf, int errcap);

void kokoro_worker_free(float* pcm);
void kokoro_worker_destroy(KokoroWorker* w);

#ifdef __cplusplus
}
#endif
