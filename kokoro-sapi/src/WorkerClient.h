#pragma once
// Client side of the synthesis pipe (see WorkerProtocol.h). The 32-bit SAPI
// engine connects to the pipe served by the running kokoro-reader app, which
// performs synthesis in its webview (WebGPU) and returns PCM. No worker process
// is spawned: if the app isn't running, the pipe is absent and EnsureConnected
// fails (the host then gets no audio for that utterance).
#include <windows.h>
#include <string>
#include <vector>

class WorkerClient {
public:
    ~WorkerClient() { Close(); }

    // Connect to the app's pipe. Returns false if nothing is serving it
    // (i.e. the kokoro-reader app isn't running).
    bool EnsureConnected();

    // Appends 24 kHz float PCM for utf8Text. `rate` is the host's rate-derived
    // speed multiplier; the app picks the narrator voice and folds in the user's
    // speed/gain itself (see WorkerProtocol.h).
    bool Synthesize(const std::string& utf8Text, float rate,
                    std::vector<float>& outSamples);

    // Ask the app for the user's current gain (volume multiplier, 1 = unity).
    // Cheap 'G' round-trip; the engine calls this at each chunk's playback start
    // so a slider move isn't frozen into already-synthesized samples. Leaves
    // outGain untouched on failure (caller keeps its last value).
    bool QueryGain(float& outGain);

    // Ask the app how many sentences to coalesce per steady-state chunk (the
    // first chunk is always one sentence for a fast start). Cheap 'C' round-trip
    // the engine issues once per Speak before splitting. Leaves outSentences
    // untouched on failure (caller keeps its built-in default).
    bool QueryChunkSentences(uint32_t& outSentences);

    void Close();

private:
    bool TryOpenPipe();

    HANDLE m_pipe = INVALID_HANDLE_VALUE;
};
