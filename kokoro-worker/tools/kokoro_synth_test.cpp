// Standalone Phase 1 driver for KokoroSynth: init the native engine and synthesize
// a multi-sentence paragraph, streaming chunk-by-chunk, to a WAV. Validates the full
// in-process path (text -> phonemes -> ORT/DML -> PCM) + sentence chunking.
//
// Usage: kokoro_synth_test.exe <model.onnx> <voice.bin> <tokenizer.json>
//                              <espeak-ng-data> <out.wav> [text]
#include "../src/KokoroSynth.h"
#include <windows.h>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <string>
#include <vector>

static std::wstring Widen(const char* s) {
    int n = MultiByteToWideChar(CP_UTF8, 0, s, -1, nullptr, 0);
    std::wstring w(n ? n - 1 : 0, L'\0');
    if (n) MultiByteToWideChar(CP_UTF8, 0, s, -1, &w[0], n);
    return w;
}

static void WriteWav(const char* path, const std::vector<float>& pcm, int sr) {
    std::vector<int16_t> s(pcm.size());
    for (size_t i = 0; i < pcm.size(); i++) {
        float v = pcm[i]; v = v < -1.f ? -1.f : (v > 1.f ? 1.f : v);
        s[i] = int16_t(v * 32767.f);
    }
    uint32_t dataBytes = uint32_t(s.size() * 2), byteRate = sr * 2;
    std::ofstream o(path, std::ios::binary);
    auto w32 = [&](uint32_t v) { o.write((char*)&v, 4); };
    auto w16 = [&](uint16_t v) { o.write((char*)&v, 2); };
    o.write("RIFF", 4); w32(36 + dataBytes); o.write("WAVE", 4);
    o.write("fmt ", 4); w32(16); w16(1); w16(1); w32(sr); w32(byteRate); w16(2); w16(16);
    o.write("data", 4); w32(dataBytes);
    o.write((char*)s.data(), dataBytes);
}

int main(int argc, char** argv) {
    if (argc < 6) { std::fprintf(stderr, "usage: model voice tokenizer espeakData out.wav [text]\n"); return 2; }
    KokoroSynth synth;
    KokoroSynth::Paths p{Widen(argv[1]), Widen(argv[2]), Widen(argv[3]), argv[4]};
    std::string err;
    LARGE_INTEGER t0, t1, freq; QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t0);
    if (!synth.Init(p, err)) { std::fprintf(stderr, "Init failed: %s\n", err.c_str()); return 1; }
    QueryPerformanceCounter(&t1);
    std::printf("init in %.2f s\n", double(t1.QuadPart - t0.QuadPart) / freq.QuadPart);

    const char* text = argc > 6 ? argv[6]
        : "Hello world. This is the native DirectML engine. It speaks without the app "
          "running. Dr. Smith read chapter 4 on page 128 for $3.50. Hmm, not bad!";
    synth.Begin(text, 1.0f, 2);

    std::vector<float> all, chunk;
    int chunks = 0;
    QueryPerformanceCounter(&t0);
    for (;;) {
        auto st = synth.Next(chunk);
        if (st == KokoroSynth::Status::End) break;
        if (st == KokoroSynth::Status::Error) { std::fprintf(stderr, "synth error at chunk %d\n", chunks); return 1; }
        all.insert(all.end(), chunk.begin(), chunk.end());
        chunks++;
        std::printf("  chunk %d: %zu samples\n", chunks, chunk.size());
    }
    QueryPerformanceCounter(&t1);
    double secs = double(all.size()) / KokoroSynth::kSampleRate;
    double wall = double(t1.QuadPart - t0.QuadPart) / freq.QuadPart;
    WriteWav(argv[5], all, KokoroSynth::kSampleRate);
    std::printf("wrote %s: %d chunks, %.2f s audio in %.2f s wall (%.2fx realtime)\n",
                argv[5], chunks, secs, wall, secs / wall);
    return 0;
}
