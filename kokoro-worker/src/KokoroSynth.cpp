#include "KokoroSynth.h"
#include "KokoroText.h"

#include <onnxruntime_cxx_api.h>

#include <espeak-ng/speak_lib.h>
#include <espeak-ng/espeak_ng.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <mutex>

namespace {

using std::string;

constexpr int kStyleDim = 256;
constexpr int kVoiceRows = 510;

// ---- half <-> float (untyped ORT tensor API; mirrors tools/synth_probe.cpp) ----
uint16_t F32ToF16(float f) {
    uint32_t x; std::memcpy(&x, &f, 4);
    uint32_t sign = (x >> 16) & 0x8000u;
    int32_t exp = int32_t((x >> 23) & 0xff) - 127 + 15;
    uint32_t mant = x & 0x7fffffu;
    if (exp <= 0) {
        if (exp < -10) return uint16_t(sign);
        mant |= 0x800000u; int shift = 14 - exp;
        uint32_t h = mant >> shift; if ((mant >> (shift - 1)) & 1u) h += 1;
        return uint16_t(sign | h);
    } else if (exp >= 31) {
        return uint16_t(sign | 0x7c00u);
    }
    uint16_t h = uint16_t(sign | (uint32_t(exp) << 10) | (mant >> 13));
    if ((mant >> 12) & 1u) h += 1;
    return h;
}
float F16ToF32(uint16_t h) {
    uint32_t sign = uint32_t(h & 0x8000u) << 16;
    uint32_t exp = (h >> 10) & 0x1fu, mant = h & 0x3ffu, f;
    if (exp == 0) {
        if (mant == 0) f = sign;
        else { exp = 127 - 15 + 1; while (!(mant & 0x400u)) { mant <<= 1; exp--; }
               mant &= 0x3ffu; f = sign | (exp << 23) | (mant << 13); }
    } else if (exp == 31) { f = sign | 0x7f800000u | (mant << 13); }
    else { f = sign | ((exp - 15 + 127) << 23) | (mant << 13); }
    float r; std::memcpy(&r, &f, 4); return r;
}

// ---- UTF-8 <-> codepoints (for the chunker, which works on Unicode scalars) ----
std::vector<char32_t> Decode(const string& s) {
    std::vector<char32_t> out; out.reserve(s.size());
    for (size_t i = 0; i < s.size();) {
        unsigned char c = s[i]; char32_t cp; int n;
        if (c < 0x80) { cp = c; n = 1; }
        else if ((c >> 5) == 0x6) { cp = c & 0x1F; n = 2; }
        else if ((c >> 4) == 0xE) { cp = c & 0x0F; n = 3; }
        else if ((c >> 3) == 0x1E) { cp = c & 0x07; n = 4; }
        else { cp = c; n = 1; }
        for (int k = 1; k < n && i + k < s.size(); k++) cp = (cp << 6) | (s[i + k] & 0x3F);
        out.push_back(cp); i += n;
    }
    return out;
}
void EncodeAppend(string& out, char32_t cp) {
    if (cp < 0x80) out += char(cp);
    else if (cp < 0x800) { out += char(0xC0 | (cp >> 6)); out += char(0x80 | (cp & 0x3F)); }
    else if (cp < 0x10000) { out += char(0xE0 | (cp >> 12)); out += char(0x80 | ((cp >> 6) & 0x3F)); out += char(0x80 | (cp & 0x3F)); }
    else { out += char(0xF0 | (cp >> 18)); out += char(0x80 | ((cp >> 12) & 0x3F)); out += char(0x80 | ((cp >> 6) & 0x3F)); out += char(0x80 | (cp & 0x3F)); }
}

// ---- sentence chunker: 1:1 port of split_text in kokoro-host/src/split_text.rs ----
std::vector<string> SplitText(const string& text, int sentencesPerChunk) {
    const int FIRST = 1;
    const int k = sentencesPerChunk < 1 ? 1 : sentencesPerChunk;
    const int SOFT_CAP = 400, HARD_CAP = 2000;
    auto c = Decode(text);
    const int n = int(c.size());
    std::vector<string> chunks;
    auto isSpace = [](char32_t ch) {
        return ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' || ch == 0x0C || ch == 0x0B;
    };
    auto isDigit = [](char32_t ch) { return ch >= '0' && ch <= '9'; };
    auto isCloser = [](char32_t ch) {
        return ch == '"' || ch == '\'' || ch == ')' || ch == ']' || ch == '}' ||
               ch == 0x201D || ch == 0x2019;
    };
    int start = 0, sentences = 0, sentenceStart = 0, lastClause = 0;
    auto flush = [&](int end) {
        int a = start, b = end;
        while (a < b && isSpace(c[a])) a++;
        while (b > a && isSpace(c[b - 1])) b--;
        if (b > a) { string s; for (int q = a; q < b; q++) EncodeAppend(s, c[q]); chunks.push_back(s); }
        start = end; sentences = 0; sentenceStart = end; lastClause = 0;
    };
    int i = 0;
    while (i < n) {
        char32_t ch = c[i];
        int boundaryEnd = 0;
        if (ch == '\n') {
            boundaryEnd = i + 1;
        } else if (ch == '.' || ch == '!' || ch == '?') {
            bool isBoundary = true;
            if (ch == '.') {
                bool decimal = i > 0 && isDigit(c[i - 1]) && i + 1 < n && isDigit(c[i + 1]);
                bool ellipsis = (i + 1 < n && c[i + 1] == '.') || (i > 0 && c[i - 1] == '.');
                if (decimal || ellipsis) isBoundary = false;
            }
            if (isBoundary) {
                int j = i + 1;
                while (j < n && (c[j] == '.' || c[j] == '!' || c[j] == '?' || isCloser(c[j]))) j++;
                if (j >= n || isSpace(c[j])) boundaryEnd = j;
            }
        }
        if (boundaryEnd != 0) {
            sentences += 1;
            int target = chunks.empty() ? FIRST : k;
            if (sentences >= target) { flush(boundaryEnd); i = start; }
            else { i = boundaryEnd; sentenceStart = boundaryEnd; lastClause = 0; }
            continue;
        }
        if ((ch == ',' || ch == ';' || ch == ':') && (i + 1 >= n || isSpace(c[i + 1])))
            lastClause = i + 1;
        if (i - sentenceStart >= SOFT_CAP && lastClause > sentenceStart) { flush(lastClause); i = start; continue; }
        if (i - sentenceStart >= HARD_CAP) {
            int brk = i;
            while (brk > start && !isSpace(c[brk - 1])) brk--;
            if (brk <= start) brk = i;
            flush(brk); i = start; continue;
        }
        i += 1;
    }
    flush(n);
    return chunks;
}

// ---- minimal tokenizer.json vocab extractor (char-level: "<utf8char>": <id>) ----
// Parses only the model.vocab object; keys may be literal UTF-8 or \uXXXX escapes.
bool LoadVocab(const std::wstring& path, std::unordered_map<string, int>& vocab) {
    std::ifstream f(path, std::ios::binary);
    if (!f) return false;
    string j((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
    size_t vp = j.find("\"vocab\"");
    if (vp == string::npos) return false;
    size_t br = j.find('{', vp);
    if (br == string::npos) return false;
    size_t i = br + 1, n = j.size();
    auto skipWs = [&]() { while (i < n && (j[i] == ' ' || j[i] == '\n' || j[i] == '\r' || j[i] == '\t')) i++; };
    while (i < n) {
        skipWs();
        if (i < n && j[i] == '}') break;
        if (j[i] != '"') { i++; continue; }
        i++;  // opening quote
        string key;
        while (i < n && j[i] != '"') {
            if (j[i] == '\\' && i + 1 < n) {
                char e = j[i + 1];
                if (e == 'u') {
                    unsigned code = std::strtoul(j.substr(i + 2, 4).c_str(), nullptr, 16);
                    EncodeAppend(key, code); i += 6;
                } else {
                    switch (e) { case 'n': key += '\n'; break; case 't': key += '\t'; break;
                                 case 'r': key += '\r'; break; default: key += e; }
                    i += 2;
                }
            } else { key += j[i]; i++; }
        }
        i++;  // closing quote
        skipWs();
        if (i < n && j[i] == ':') i++;
        skipWs();
        int val = std::atoi(j.c_str() + i);
        while (i < n && j[i] != ',' && j[i] != '}') i++;
        if (i < n && j[i] == ',') i++;
        vocab[key] = val;
    }
    return !vocab.empty();
}

std::once_flag g_espeakOnce;
bool g_espeakOk = false;

}  // namespace

struct KokoroSynth::Impl {
    // Created lazily in Init() — NOT at construction. A global NativeSynthSource holds
    // a KokoroSynth, so its Impl is built during DLL static init, before DllMain
    // preloads onnxruntime.dll. Constructing Ort::Env there would call the not-yet-
    // loaded (delay-loaded) ORT and fail DLL init with 0x45A (ERROR_DLL_INIT_FAILED).
    std::unique_ptr<Ort::Env> env;
    std::unique_ptr<Ort::Session> session;
    std::vector<string> inNames, outNames;
    std::vector<ONNXTensorElementDataType> inTypes;
    ONNXTensorElementDataType outType = ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16;

    std::unordered_map<string, int> vocab;
    std::vector<float> voice;  // kVoiceRows * kStyleDim

    std::vector<string> chunks;
    size_t chunkIdx = 0;
    float speed = 1.0f;
    std::atomic<bool> cancel{false};
};

KokoroSynth::KokoroSynth() : m_(std::make_unique<Impl>()) {}
KokoroSynth::~KokoroSynth() = default;

bool KokoroSynth::Init(const Paths& paths, std::string& err) {
    // espeak: one global init per process (SYNCHRONOUS output so Synth is blocking,
    // audio discarded via callback — we only want the phoneme trace).
    std::call_once(g_espeakOnce, [&] {
        espeak_ng_InitializePath(paths.espeakDataDir.c_str());
        espeak_ng_ERROR_CONTEXT ctx = nullptr;
        if (espeak_ng_Initialize(&ctx) != ENS_OK) return;
        if (espeak_ng_InitializeOutput(ENOUTPUT_MODE_SYNCHRONOUS, 0, nullptr) != ENS_OK) return;
        espeak_SetSynthCallback([](short*, int, espeak_EVENT*) { return 0; });
        if (espeak_SetVoiceByName("en-us") != EE_OK) return;
        g_espeakOk = true;
    });
    if (!g_espeakOk) { err = "espeak init failed"; return false; }

    if (!LoadVocab(paths.tokenizerJson, m_->vocab)) { err = "tokenizer vocab load failed"; return false; }

    // voice .bin: kVoiceRows x kStyleDim float32.
    {
        std::ifstream vf(paths.voiceBin, std::ios::binary);
        if (!vf) { err = "voice .bin open failed"; return false; }
        m_->voice.resize(size_t(kVoiceRows) * kStyleDim);
        vf.read(reinterpret_cast<char*>(m_->voice.data()), std::streamsize(m_->voice.size() * sizeof(float)));
        if (!vf) { err = "voice .bin read short"; return false; }
    }

    try {
        m_->env = std::make_unique<Ort::Env>(ORT_LOGGING_LEVEL_WARNING, "kokoro");
        Ort::SessionOptions so;
        so.DisableMemPattern();
        so.SetExecutionMode(ORT_SEQUENTIAL);
        // Native Dawn-embedded WebGPU EP (generic named-EP append; same EP the Python
        // onnxruntime-webgpu probe used as "WebGpuExecutionProvider"). Falls back to the
        // CPU EP automatically for any node WebGPU can't take. Stock fp32 model.onnx runs
        // as-is (no patch_dml); the RunModel dtype branch already handles fp32 I/O.
        so.AppendExecutionProvider("WebGPU", {});
        m_->session = std::make_unique<Ort::Session>(*m_->env, paths.modelOnnx.c_str(), so);

        Ort::AllocatorWithDefaultOptions alloc;
        size_t nIn = m_->session->GetInputCount(), nOut = m_->session->GetOutputCount();
        for (size_t k = 0; k < nIn; k++) {
            m_->inNames.push_back(m_->session->GetInputNameAllocated(k, alloc).get());
            m_->inTypes.push_back(m_->session->GetInputTypeInfo(k).GetTensorTypeAndShapeInfo().GetElementType());
        }
        for (size_t k = 0; k < nOut; k++)
            m_->outNames.push_back(m_->session->GetOutputNameAllocated(k, alloc).get());
        m_->outType = m_->session->GetOutputTypeInfo(0).GetTensorTypeAndShapeInfo().GetElementType();
    } catch (const Ort::Exception& e) {
        err = string("ORT/WebGPU init failed: ") + e.what();
        return false;
    }
    return true;
}

void KokoroSynth::Begin(const std::string& utf8Text, float speed, int sentencesPerChunk) {
    m_->chunks = SplitText(utf8Text, sentencesPerChunk);
    m_->chunkIdx = 0;
    m_->speed = speed;
    m_->cancel.store(false);
}

void KokoroSynth::Cancel() { m_->cancel.store(true); }

// espeak synth-trace phonemization of one punctuation-free segment (the path
// phonemizer/kokoro-js uses; assigns interjection stress TextToPhonemes drops).
static std::string PhonemizeSegment(const std::string& text) {
    char tmp[L_tmpnam_s];
    if (tmpnam_s(tmp, L_tmpnam_s) != 0) return "";
    FILE* f = std::fopen(tmp, "wb+");
    if (!f) return "";
    espeak_SetPhonemeTrace(0x02, f);  // bit1 = IPA
    espeak_Synth(text.c_str(), text.size() + 1, 0, POS_CHARACTER, 0,
                 espeakCHARS_UTF8 | espeakPHONEMES, nullptr, nullptr);
    espeak_SetPhonemeTrace(0x02, nullptr);
    std::fflush(f);
    std::fseek(f, 0, SEEK_END); long sz = std::ftell(f); std::fseek(f, 0, SEEK_SET);
    string buf(sz > 0 ? size_t(sz) : 0, 0);
    if (sz > 0) { size_t rd = std::fread(&buf[0], 1, size_t(sz), f); buf.resize(rd); }
    std::fclose(f); std::remove(tmp);
    string p;  // one clause per line -> join with a single space
    for (char ch : buf) {
        if (ch == '\n' || ch == '\r' || ch == '\t') { if (!p.empty() && p.back() != ' ') p += ' '; }
        else p += ch;
    }
    while (!p.empty() && p.back() == ' ') p.pop_back();
    return p;
}

std::string KokoroSynth::Phonemize(const std::string& text) {
    string norm = kokoro_text::Normalize(text);
    auto segs = kokoro_text::SplitSegments(norm);
    string joined;
    for (const auto& seg : segs)
        joined += seg.isPunct ? seg.text : PhonemizeSegment(seg.text);
    return kokoro_text::PostProcess(joined);
}

std::vector<int64_t> KokoroSynth::Tokenize(const std::string& phon) {
    std::vector<int64_t> ids;
    ids.push_back(0);  // BOS
    for (size_t i = 0; i < phon.size();) {
        unsigned char c = phon[i];
        int n = c < 0x80 ? 1 : (c >> 5) == 0x6 ? 2 : (c >> 4) == 0xE ? 3 : (c >> 3) == 0x1E ? 4 : 1;
        auto it = m_->vocab.find(phon.substr(i, n));
        if (it != m_->vocab.end()) ids.push_back(it->second);
        i += n;
    }
    ids.push_back(0);  // EOS
    return ids;
}

bool KokoroSynth::RunModel(const std::vector<int64_t>& ids, float speed,
                           std::vector<float>& outPcm, std::string& err) {
    try {
        // style row = min(max(numTokens-2,0),509) (kokoro-js generate_from_ids).
        int row = int(ids.size()) - 2;
        if (row < 0) row = 0; if (row > kVoiceRows - 1) row = kVoiceRows - 1;
        const float* styleF = m_->voice.data() + size_t(row) * kStyleDim;

        auto mem = Ort::MemoryInfo::CreateCpu(OrtArenaAllocator, OrtMemTypeDefault);
        std::vector<int64_t> idbuf = ids;
        std::vector<float> styleBuf(styleF, styleF + kStyleDim), speedBuf{speed};
        std::vector<uint16_t> styleH, speedH;  // fp16 backing, if the model wants it

        std::vector<Ort::Value> inputs;
        std::vector<const char*> inPtrs;
        for (size_t k = 0; k < m_->inNames.size(); k++) {
            const string& nm = m_->inNames[k];
            inPtrs.push_back(nm.c_str());
            const bool f16 = m_->inTypes[k] == ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16;
            if (nm == "input_ids" || nm == "tokens") {
                int64_t shape[2] = {1, int64_t(idbuf.size())};
                inputs.push_back(Ort::Value::CreateTensor<int64_t>(mem, idbuf.data(), idbuf.size(), shape, 2));
            } else if (nm == "style" || nm == "ref_s") {
                int64_t shape[2] = {1, kStyleDim};
                if (f16) {
                    styleH.resize(kStyleDim);
                    for (int q = 0; q < kStyleDim; q++) styleH[q] = F32ToF16(styleBuf[q]);
                    inputs.push_back(Ort::Value::CreateTensor(mem, styleH.data(), styleH.size() * 2, shape, 2,
                                     ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16));
                } else {
                    inputs.push_back(Ort::Value::CreateTensor<float>(mem, styleBuf.data(), styleBuf.size(), shape, 2));
                }
            } else {  // speed
                int64_t shape[1] = {1};
                if (f16) {
                    speedH.resize(1); speedH[0] = F32ToF16(speed);
                    inputs.push_back(Ort::Value::CreateTensor(mem, speedH.data(), 2, shape, 1,
                                     ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16));
                } else {
                    inputs.push_back(Ort::Value::CreateTensor<float>(mem, speedBuf.data(), 1, shape, 1));
                }
            }
        }
        std::vector<const char*> outPtrs;
        for (auto& o : m_->outNames) outPtrs.push_back(o.c_str());

        auto outs = m_->session->Run(Ort::RunOptions{nullptr}, inPtrs.data(), inputs.data(),
                                     inputs.size(), outPtrs.data(), outPtrs.size());
        auto info = outs[0].GetTensorTypeAndShapeInfo();
        size_t nSamp = info.GetElementCount();
        outPcm.resize(nSamp);
        if (info.GetElementType() == ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT16) {
            const uint16_t* p = outs[0].GetTensorData<uint16_t>();
            for (size_t q = 0; q < nSamp; q++) outPcm[q] = F16ToF32(p[q]);
        } else {
            const float* p = outs[0].GetTensorData<float>();
            std::memcpy(outPcm.data(), p, nSamp * sizeof(float));
        }
        return true;
    } catch (const Ort::Exception& e) {
        err = string("ORT run failed: ") + e.what();
        return false;
    }
}

bool KokoroSynth::SynthOne(const std::string& utf8Text, float speed,
                           std::vector<float>& outPcm, std::string& err) {
    outPcm.clear();
    string phon = Phonemize(utf8Text);
    std::vector<int64_t> ids = Tokenize(phon);
    if (std::getenv("KOKORO_DUMP_TOKENS")) {
        std::fprintf(stderr, "PHON: %s\nTOKENS:", phon.c_str());
        for (int64_t id : ids) std::fprintf(stderr, " %lld", (long long)id);
        std::fprintf(stderr, "\n");
    }
    if (ids.size() <= 2) return true;  // empty / punctuation-only: nothing to say
    return RunModel(ids, speed, outPcm, err);
}

bool KokoroSynth::SetVoice(const std::wstring& voiceBin, std::string& err) {
    std::ifstream vf(voiceBin, std::ios::binary);
    if (!vf) { err = "voice .bin open failed"; return false; }
    std::vector<float> v(size_t(kVoiceRows) * kStyleDim);
    vf.read(reinterpret_cast<char*>(v.data()), std::streamsize(v.size() * sizeof(float)));
    if (!vf) { err = "voice .bin read short"; return false; }
    m_->voice.swap(v);  // commit only on full success (keeps current voice otherwise)
    return true;
}

KokoroSynth::Status KokoroSynth::Next(std::vector<float>& outPcm) {
    outPcm.clear();
    if (m_->cancel.load()) return Status::End;
    if (m_->chunkIdx >= m_->chunks.size()) return Status::End;

    const string& chunk = m_->chunks[m_->chunkIdx++];
    string phon = Phonemize(chunk);
    std::vector<int64_t> ids = Tokenize(phon);
    if (std::getenv("KOKORO_DUMP_TOKENS")) {
        std::fprintf(stderr, "PHON: %s\nTOKENS:", phon.c_str());
        for (int64_t id : ids) std::fprintf(stderr, " %lld", (long long)id);
        std::fprintf(stderr, "\n");
    }
    if (ids.size() <= 2) return Next(outPcm);  // empty chunk (punctuation only): skip

    std::string err;
    if (!RunModel(ids, m_->speed, outPcm, err)) return Status::Error;
    return Status::Data;
}
