#pragma once
// The native synthesis core for the WebGPU worker: text -> 24 kHz mono PCM. Owns the
// espeak session (phonemization), the ONNX Runtime + native Dawn WebGPU session (the
// Kokoro model), the tokenizer vocab, and the voice style vectors. Streams a whole
// utterance as a sequence of sentence chunks. (Recovered from the DirectML edition's
// KokoroSynth; only the EP + model differ — see KokoroSynth.cpp Init.)
//
// Pipeline per chunk (see KokoroText):
//   normalize -> segment -> espeak synth-trace phonemes -> post-process -> tokens
//   -> ORT/WebGPU run (input_ids, style row, speed) -> f32 waveform.
//
// Validated: tokens are token-exact vs kokoro-js (per chunk); WebGPU audio matches the
// kokoro-js CPU baseline (spec-corr ~0.999). This class assembles them behind a
// streaming API so the Rust pipe host can drive it (Phase 2), x86 DLL untouched.
#include <atomic>
#include <cstdint>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

class KokoroSynth {
public:
    KokoroSynth();
    ~KokoroSynth();

    // Absolute paths to the assets. espeakData is the bundled espeak-ng-data dir;
    // the rest live in the shared model dir. Returns false + fills `err` on failure
    // (missing file, DirectML unavailable, bad model).
    struct Paths {
        std::wstring modelOnnx;      // stock model.onnx (fp32; runs on the WebGPU EP)
        std::wstring voiceBin;       // voices/<narrator>.bin (510x256 f32)
        std::wstring tokenizerJson;  // tokenizer.json (char-level IPA vocab)
        std::string  espeakDataDir;  // espeak-ng-data (utf8 path for espeak API)
    };
    bool Init(const Paths& paths, std::string& err);

    // Begin an utterance: chunk `utf8Text` into sentences and reset streaming state.
    // `speed` is the synthesis speed (1 = normal; the SAPI rate maps into this).
    // `sentencesPerChunk` mirrors the app's "tts-chunk" (first chunk is 1 sentence
    // for a fast start). Cheap — no synthesis happens until Next().
    void Begin(const std::string& utf8Text, float speed, int sentencesPerChunk = 2);

    enum class Status { Data, End, Error };
    // Synthesize the next chunk into `outPcm` (24 kHz mono f32, [-1,1]). Returns
    // Data while chunks remain, End when the utterance is done, Error on failure.
    // Honors Cancel() between chunks.
    Status Next(std::vector<float>& outPcm);

    // Atomically request cancellation of the in-flight utterance (safe from another
    // thread — the SAPI engine calls this on stop). The next Next() returns End.
    void Cancel();

    static constexpr int kSampleRate = 24000;

private:
    std::string Phonemize(const std::string& text);           // full text->IPA pipeline
    std::vector<int64_t> Tokenize(const std::string& phon);   // IPA -> ids (BOS/EOS=0)
    bool RunModel(const std::vector<int64_t>& ids, float speed,
                  std::vector<float>& outPcm, std::string& err);

    struct Impl;
    std::unique_ptr<Impl> m_;
};
