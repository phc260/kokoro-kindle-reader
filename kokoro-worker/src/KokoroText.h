#pragma once
// Native C++ port of kokoro-js's text->phoneme normalization pipeline `m()`, so
// the DirectML edition feeds the Kokoro model the SAME tokens as the WebView2
// edition. See tools/kokoro-phonemize-spec.md for the verbatim JS source this
// mirrors, and tools/phonemize_test.cpp for the parity harness (vs phon_ref.mjs).
//
// The espeak call itself lives in the caller (KokoroSynth owns the espeak session);
// this unit is the pure-string normalization + segmentation + phoneme post-map
// around it. Everything is UTF-8.
#include <string>
#include <vector>

namespace kokoro_text {

// Stage 1: normalize raw text (quotes/punct folding, Dr./Mr., numbers, currency,
// years, times, ranges, possessives, acronyms).
std::string Normalize(const std::string& utf8);

// Stage 2: split normalized text into alternating segments. `isPunct` runs (of
// `;:,.!?¡¿—…"«»“”(){}[]` + surrounding spaces) are kept literally; the rest is
// phonemized by the caller (espeak) and the clause outputs joined with a space.
struct Segment { bool isPunct; std::string text; };
std::vector<Segment> SplitSegments(const std::string& normalized);

// Stage 3: post-process the concatenated phoneme string (Kokoro remap, fold
// ʲ/r/x/ɬ into Kokoro's vocab, "hundred" spacing, stray-z fix, en-us "ninety").
std::string PostProcess(const std::string& phonemes);

}  // namespace kokoro_text
