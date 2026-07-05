#include "kokoro_ffi.h"
#include "KokoroSynth.h"

#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

struct KokoroWorker {
    KokoroSynth synth;
};

namespace {
std::wstring Wide(const uint16_t* s) {
    // NUL-terminated UTF-16LE -> std::wstring (wchar_t is 16-bit on Windows).
    return s ? std::wstring(reinterpret_cast<const wchar_t*>(s)) : std::wstring();
}
void SetErr(char* buf, int cap, const std::string& msg) {
    if (!buf || cap <= 0) return;
    int n = int(msg.size());
    if (n > cap - 1) n = cap - 1;
    std::memcpy(buf, msg.data(), size_t(n));
    buf[n] = '\0';
}
}  // namespace

extern "C" KokoroWorker* kokoro_worker_create(const uint16_t* modelOnnx, const uint16_t* voiceBin,
                                              const uint16_t* tokenizerJson, const char* espeakDataDir,
                                              char* errbuf, int errcap) {
    KokoroWorker* w = new KokoroWorker();
    KokoroSynth::Paths p{Wide(modelOnnx), Wide(voiceBin), Wide(tokenizerJson),
                         espeakDataDir ? std::string(espeakDataDir) : std::string()};
    std::string err;
    if (!w->synth.Init(p, err)) {
        SetErr(errbuf, errcap, err);
        delete w;
        return nullptr;
    }
    return w;
}

extern "C" int64_t kokoro_worker_synth(KokoroWorker* w, const char* utf8Text, float speed,
                                       float** outPcm, char* errbuf, int errcap) {
    if (outPcm) *outPcm = nullptr;
    if (!w) { SetErr(errbuf, errcap, "null worker"); return -1; }
    std::vector<float> pcm;
    std::string err;
    if (!w->synth.SynthOne(utf8Text ? utf8Text : "", speed, pcm, err)) {
        SetErr(errbuf, errcap, err);
        return -1;
    }
    if (outPcm && !pcm.empty()) {
        size_t bytes = pcm.size() * sizeof(float);
        float* buf = static_cast<float*>(std::malloc(bytes));
        if (!buf) { SetErr(errbuf, errcap, "malloc failed"); return -1; }
        std::memcpy(buf, pcm.data(), bytes);
        *outPcm = buf;
    }
    return int64_t(pcm.size());
}

extern "C" int kokoro_worker_set_voice(KokoroWorker* w, const uint16_t* voiceBin, char* errbuf, int errcap) {
    if (!w) { SetErr(errbuf, errcap, "null worker"); return -1; }
    std::string err;
    if (!w->synth.SetVoice(Wide(voiceBin), err)) {
        SetErr(errbuf, errcap, err);
        return -1;
    }
    return 0;
}

extern "C" void kokoro_worker_free(float* pcm) { std::free(pcm); }

extern "C" void kokoro_worker_destroy(KokoroWorker* w) { delete w; }
