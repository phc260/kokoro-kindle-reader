// Utterance → sentence-chunk splitter for the headless host's pipe path. The
// correctness-critical chunking. Pure (no external deps).

/// Split an utterance into sentence chunks for streaming. We ramp up: the FIRST
/// chunk is a single sentence (so audio starts quickly), then chunks coalesce
/// `sentences_per_chunk` sentences each (fewer round-trips / inter-chunk seams).
/// Boundaries are `. ! ?` (followed by whitespace / a closing quote / end) and
/// newlines; decimals ("3.14") and ellipses are not boundaries. A single sentence
/// that runs past `SOFT_CAP` is split at its last clause boundary (`, ; :`) so a
/// run-on breaks at a natural pause; only if it has no clause boundary at all does
/// it fall back to a word break past `HARD_CAP`. The frontend (kokoro-js)
/// sub-splits anything past its token limit anyway. Ported from the old
/// KokoroTTSEngine.cpp `SplitText`; operates on chars (Unicode scalars).
pub fn split_text(text: &str, sentences_per_chunk: usize) -> Vec<String> {
    const FIRST_SENTENCES: usize = 1; // small first chunk -> each page starts fast
    let k_sentences = sentences_per_chunk.max(1); // 0 would never flush
    const SOFT_CAP: usize = 400; // over-long sentence: break at a clause (, ; :)
    const HARD_CAP: usize = 2000; // last resort (no clause found: word boundary)

    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut chunks: Vec<String> = Vec::new();

    let is_space =
        |ch: char| matches!(ch, ' ' | '\t' | '\r' | '\n' | '\u{0C}' | '\u{0B}');
    let is_digit = |ch: char| ch.is_ascii_digit();
    // closing quotes/brackets, incl. curly ” ’
    let is_closer =
        |ch: char| matches!(ch, '"' | '\'' | ')' | ']' | '}' | '\u{201D}' | '\u{2019}');

    // `sentence_start` tracks the in-progress sentence so the caps measure one
    // sentence, not the whole multi-sentence chunk. `last_clause` is the position
    // just after the most recent `, ; :` in that sentence — the preferred split
    // point for an over-long one.
    let mut start = 0usize;
    let mut sentences = 0usize;
    let mut sentence_start = 0usize;
    let mut last_clause = 0usize;

    // flush takes the mutable state by ref to dodge closure/borrow conflicts.
    let flush = |chunks: &mut Vec<String>,
                 start: &mut usize,
                 sentences: &mut usize,
                 sentence_start: &mut usize,
                 last_clause: &mut usize,
                 end: usize| {
        let mut a = *start;
        let mut b = end;
        while a < b && is_space(c[a]) {
            a += 1;
        }
        while b > a && is_space(c[b - 1]) {
            b -= 1;
        }
        if b > a {
            chunks.push(c[a..b].iter().collect());
        }
        *start = end;
        *sentences = 0;
        *sentence_start = end;
        *last_clause = 0;
    };

    let mut i = 0usize;
    while i < n {
        let ch = c[i];

        // Find a sentence/paragraph boundary at i; boundary_end = position after it.
        let mut boundary_end = 0usize;
        if ch == '\n' {
            boundary_end = i + 1;
        } else if ch == '.' || ch == '!' || ch == '?' {
            let mut is_boundary = true;
            if ch == '.' {
                let decimal =
                    i > 0 && is_digit(c[i - 1]) && i + 1 < n && is_digit(c[i + 1]);
                let ellipsis =
                    (i + 1 < n && c[i + 1] == '.') || (i > 0 && c[i - 1] == '.');
                if decimal || ellipsis {
                    is_boundary = false;
                }
            }
            if is_boundary {
                let mut j = i + 1; // swallow trailing terminators + closers (?!" or .")
                while j < n
                    && (c[j] == '.' || c[j] == '!' || c[j] == '?' || is_closer(c[j]))
                {
                    j += 1;
                }
                if j >= n || is_space(c[j]) {
                    boundary_end = j;
                }
            }
        }

        if boundary_end != 0 {
            // Count the sentence; emit once we've collected `target` of them. `target`
            // ramps 1, 2, 4, ... up to k_sentences: a tiny first chunk starts audio fast,
            // and doubling each chunk builds a playback buffer before big chunks so the
            // synth pipeline never starves (a small first chunk followed straight by a big
            // one leaves a silent gap after the first sentence while the big one renders).
            sentences += 1;
            let target = (1usize << chunks.len().min(20)).min(k_sentences).max(FIRST_SENTENCES);
            if sentences >= target {
                flush(
                    &mut chunks,
                    &mut start,
                    &mut sentences,
                    &mut sentence_start,
                    &mut last_clause,
                    boundary_end,
                );
                i = start;
            } else {
                i = boundary_end;
                sentence_start = boundary_end; // next sentence begins here
                last_clause = 0;
            }
            continue;
        }

        // Remember clause boundaries (`, ; :` before whitespace / end) as
        // candidate split points for an over-long sentence.
        if (ch == ',' || ch == ';' || ch == ':') && (i + 1 >= n || is_space(c[i + 1])) {
            last_clause = i + 1;
        }

        // The current sentence has run long: prefer to break at its last clause
        // boundary; fall back to a word break only if it has none (HARD_CAP).
        if i - sentence_start >= SOFT_CAP && last_clause > sentence_start {
            let at = last_clause; // copy: can't pass &mut last_clause and it by value
            flush(
                &mut chunks,
                &mut start,
                &mut sentences,
                &mut sentence_start,
                &mut last_clause,
                at,
            );
            i = start;
            continue;
        }
        if i - sentence_start >= HARD_CAP {
            // no clause break: cut on a word boundary
            let mut brk = i;
            while brk > start && !is_space(c[brk - 1]) {
                brk -= 1;
            }
            if brk <= start {
                brk = i; // one long token: hard cut
            }
            flush(
                &mut chunks,
                &mut start,
                &mut sentences,
                &mut sentence_start,
                &mut last_clause,
                brk,
            );
            i = start;
            continue;
        }
        i += 1;
    }
    flush(
        &mut chunks,
        &mut start,
        &mut sentences,
        &mut sentence_start,
        &mut last_clause,
        n,
    ); // trailing text
    chunks
}
