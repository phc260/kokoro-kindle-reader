// Kokoro-js text normalization + punctuation segmentation + phoneme post-processing.
// Operates on UTF-8 bytes (Vec<u8>/&[u8]) so the byte-scanning passes mirror the
// upstream kokoro-js `m()` regexes exactly. Verified by token-parity against kokoro-js.
// The explicit ASCII-range checks below are deliberate (they trace the JS char classes),
// so the manual-ascii-check lint is allowed.
#![allow(clippy::needless_range_loop, clippy::manual_is_ascii_check)]

// ---- small helpers (bytes) --------------------------------------------------
fn is_digit(c: u8) -> bool { c.is_ascii_digit() }
fn is_alpha(c: u8) -> bool { c.is_ascii_alphabetic() }
fn is_upper(c: u8) -> bool { c.is_ascii_uppercase() }
fn is_word(c: u8) -> bool { is_alpha(c) || is_digit(c) || c == b'_' }
fn lower(c: u8) -> u8 { if c.is_ascii_uppercase() { c - b'A' + b'a' } else { c } }

/// s[i..i+pat.len()] == pat, with bounds.
fn eq_at(s: &[u8], i: usize, pat: &[u8]) -> bool {
    i + pat.len() <= s.len() && &s[i..i + pat.len()] == pat
}

fn find_from(s: &[u8], pat: &[u8], start: usize) -> Option<usize> {
    if pat.is_empty() || pat.len() > s.len() { return None; }
    let mut i = start;
    while i + pat.len() <= s.len() {
        if &s[i..i + pat.len()] == pat { return Some(i); }
        i += 1;
    }
    None
}

/// C `atoi`: skip ws/sign, parse leading digits.
fn atoi(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') { i += 1; }
    let mut sign = 1i64;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        if s[i] == b'-' { sign = -1; }
        i += 1;
    }
    let mut n = 0i64;
    while i < s.len() && s[i].is_ascii_digit() {
        n = n * 10 + (s[i] - b'0') as i64;
        i += 1;
    }
    sign * n
}

fn itoa(n: i64) -> Vec<u8> { n.to_string().into_bytes() }

// A UTF-8 codepoint's byte length from its lead byte.
fn utf8_len(c: u8) -> usize {
    if c < 0x80 { 1 } else if (c >> 5) == 0x6 { 2 } else if (c >> 4) == 0xE { 3 } else if (c >> 3) == 0x1E { 4 } else { 1 }
}

// ---- source spans -----------------------------------------------------------
// Every stage below carries, for each byte it emits, the range of the ORIGINAL text that
// byte came from. That chain is what lets a phoneme — and so a model-predicted duration —
// be pointed back at the source word a highlight has to address. Without it the only
// thing relating audio to text is the character fraction the SAPI engine currently
// interpolates, which is an estimate delivered exactly, not an alignment.
//
// The rule for an expansion is that it collapses: every byte of `19 77` carries the span
// of `1977`, and every byte of `3 dollars and 50 cents` carries the span of `$3.50`. A
// source token that becomes several spoken words still gets ONE highlight, held for as
// long as the expansion is being spoken.

/// Where an output byte came from: a half-open **byte** range of the original text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// The identity map for an `n`-byte input: byte `i` came from byte `i`. The seed the
/// pipeline starts from, and what a caller passes when its input *is* the original text.
pub fn identity_spans(n: usize) -> Vec<Span> {
    (0..n as u32).map(|i| Span { start: i, end: i + 1 }).collect()
}

/// Byte builder that records a [`Span`] per byte it accumulates.
///
/// `src` maps this pass's INPUT bytes to original spans, so a pass records provenance in
/// original coordinates directly. Composing per-stage maps afterwards would work too, but
/// every stage would then be one more place to get the composition wrong.
struct Tracked<'a> {
    src: &'a [Span],
    out: Vec<u8>,
    map: Vec<Span>,
}

impl<'a> Tracked<'a> {
    fn new(src: &'a [Span]) -> Self {
        Tracked { src, out: Vec::with_capacity(src.len()), map: Vec::with_capacity(src.len()) }
    }

    /// The union of the source spans of input bytes `[start, end)`. Spans only ever move
    /// forward through the input, so the union is the first byte's start to the last
    /// byte's end — no scan needed. An out-of-range index degenerates to the end of the
    /// input rather than panicking: a bad span costs a misplaced highlight, and taking
    /// down the synth worker inside Kindle costs the rest of the book.
    fn span(&self, start: usize, end: usize) -> Span {
        let tail = self.src.last().map(|s| s.end).unwrap_or(0);
        let lo = self.src.get(start).map(|s| s.start).unwrap_or(tail);
        let hi = end
            .checked_sub(1)
            .and_then(|i| self.src.get(i))
            .map(|s| s.end)
            .unwrap_or(lo);
        Span { start: lo, end: hi.max(lo) }
    }

    /// One input byte, passed through (possibly substituted 1:1) as `c`.
    fn keep(&mut self, c: u8, at: usize) {
        let sp = self.span(at, at + 1);
        self.out.push(c);
        self.map.push(sp);
    }

    /// A verbatim run of the input, byte for byte.
    fn copy(&mut self, s: &[u8], start: usize, end: usize) {
        for i in start..end.min(s.len()) {
            self.keep(s[i], i);
        }
    }

    /// `bytes` stands in for the input range `[start, end)` — every emitted byte carries
    /// the whole range. This is the collapsing rule above.
    fn emit(&mut self, bytes: &[u8], start: usize, end: usize) {
        let sp = self.span(start, end);
        for &b in bytes {
            self.out.push(b);
            self.map.push(sp);
        }
    }

    fn last(&self) -> Option<u8> {
        self.out.last().copied()
    }

    fn ends_with(&self, pat: &[u8]) -> bool {
        self.out.len() >= pat.len() && &self.out[self.out.len() - pat.len()..] == pat
    }

    fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    fn into_parts(self) -> (Vec<u8>, Vec<Span>) {
        (self.out, self.map)
    }
}

/// `replace_all` over a byte buffer and its span map together. Every byte of `to`
/// inherits the span of the whole `from` occurrence, so a substitution can never leave
/// half a source token pointing somewhere else.
fn replace_all_spans(s: &mut Vec<u8>, map: &mut Vec<Span>, from: &[u8], to: &[u8]) {
    if from.is_empty() { return; }
    let mut p = 0;
    while let Some(idx) = find_from(s, from, p) {
        let sp = Span { start: map[idx].start, end: map[idx + from.len() - 1].end };
        s.splice(idx..idx + from.len(), to.iter().copied());
        map.splice(idx..idx + from.len(), std::iter::repeat_n(sp, to.len()));
        p = idx + to.len();
    }
}

/// Trim ASCII whitespace-and-below from both ends, taking the span map with it.
fn trim_spans(s: &[u8], map: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut a = 0;
    let mut b = s.len();
    while a < b && s[a] <= b' ' { a += 1; }
    while b > a && s[b - 1] <= b' ' { b -= 1; }
    (s[a..b].to_vec(), map[a..b].to_vec())
}

/// UTF-16 code-unit offset at every byte boundary of `utf8` (length `utf8.len() + 1`).
///
/// A [`Span`] counts bytes because that is what the normalization passes scan; a mark has
/// to count UTF-16 code units because that is what SAPI hands Kindle. This is the one
/// conversion between them, and it is done against the original text so a non-BMP
/// character costs its two code units exactly where it sits. A byte in the middle of a
/// codepoint reports its lead byte's offset, so a span that lands mid-character still
/// resolves rather than panicking.
pub fn utf16_offsets(utf8: &[u8]) -> Vec<u32> {
    let mut off: Vec<u32> = Vec::with_capacity(utf8.len() + 1);
    let mut units = 0u32;
    let mut i = 0;
    while i < utf8.len() {
        let n = utf8_len(utf8[i]);
        for _ in 0..n.min(utf8.len() - i) {
            off.push(units);
        }
        units += if n == 4 { 2 } else { 1 }; // non-BMP arrives as a surrogate pair
        i += n;
    }
    off.push(units);
    off
}

/// Lift a span expressed in a *slice* of some mapped text back to source coordinates.
///
/// `spans` maps that text's bytes to the source; `offset` is where the slice starts in it;
/// `local` is the span in the slice's own bytes. This is the join between espeak (which
/// reports positions inside the one segment it was handed) and normalization (which maps
/// the whole normalized text back to the original) — the only arithmetic in the chain that
/// has an offset on both sides of it, so it is worth having on its own.
///
/// `None` when `local` doesn't resolve, which means the reporter pointed outside the text
/// it was given. The caller decides what a coarser answer should be; there isn't one right
/// fallback, and inventing a span here would hide the fact that one was needed.
pub fn lift(spans: &[Span], offset: usize, local: Span) -> Option<Span> {
    let a = offset.checked_add(local.start as usize)?;
    let b = offset.checked_add(local.end as usize)?;
    let lo = spans.get(a)?.start;
    let hi = spans.get(b.checked_sub(1)?)?.end;
    Some(Span { start: lo, end: hi.max(lo) })
}

/// Widen a span to whole characters of `source`.
///
/// The passes here scan bytes, so a multi-byte character can end up with a span per byte —
/// an em-dash becomes three of them, two covering no character at all. Those convert to
/// empty UTF-16 ranges, and a mark with a zero-length character span is rejected outright.
/// Snapping is what keeps a character indivisible on the way out, which is the only way
/// the rest of the pipeline sees it.
pub fn snap_to_chars(span: Span, source: &[u8]) -> Span {
    let is_cont = |c: u8| (c & 0xC0) == 0x80;
    let mut a = (span.start as usize).min(source.len());
    while a > 0 && a < source.len() && is_cont(source[a]) {
        a -= 1;
    }
    let mut b = (span.end as usize).min(source.len()).max(a);
    while b < source.len() && is_cont(source[b]) {
        b += 1;
    }
    if b == a && a < source.len() {
        b = (a + utf8_len(source[a])).min(source.len());
    }
    Span { start: a as u32, end: b as u32 }
}

/// Widen a span to the whitespace-delimited word of `source` that it sits in.
///
/// A highlight addresses a word on the page, and a page word is what lies between spaces —
/// `state-of-the-art` and `$3.50` and `1,250` are each one of them. Snapping outward is
/// what makes several reports of the same word agree so they can be merged: espeak splits
/// `1,250` into `1` and `250` (the normalizer having dropped the comma), and those two
/// spans do not overlap until both are widened to the word they came from.
pub fn word_bounds(source: &[u8], span: Span) -> Span {
    let ws = |c: u8| c == b' ' || c == b'\t' || c == b'\n' || c == b'\r';
    let lo = (span.start as usize).min(source.len());
    let hi = (span.end as usize).min(source.len()).max(lo);
    // Shrink to the span's own non-whitespace content FIRST. espeak's word extents are
    // built by tiling, so each runs up to where the next word begins and carries the
    // separating space with it — widening from that end would swallow the next word and
    // fold the whole line into one mark.
    let mut a = lo;
    let mut b = hi;
    while a < b && ws(source[a]) {
        a += 1;
    }
    while b > a && ws(source[b - 1]) {
        b -= 1;
    }
    if a == b {
        return snap_to_chars(span, source); // all whitespace: there is no word to widen to
    }
    // Then widen to the page word. Each loop stops just after a whitespace byte, which is
    // always a character boundary.
    while a > 0 && !ws(source[a - 1]) {
        a -= 1;
    }
    while b < source.len() && !ws(source[b]) {
        b += 1;
    }
    snap_to_chars(Span { start: a as u32, end: b as u32 }, source)
}

/// Whether a span of `source` is empty or nothing but whitespace.
fn is_blank(source: &[u8], span: Span) -> bool {
    let a = (span.start as usize).min(source.len());
    let b = (span.end as usize).min(source.len()).max(a);
    source[a..b].iter().all(|&c| c == b' ' || c == b'\t' || c == b'\n' || c == b'\r')
}

/// One aggregated stretch of speech: the source it covers and the samples it occupies.
/// [`aggregate_spans`] produces these; the synth layer turns them into its own mark type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimedSpan {
    pub span: Span,
    pub sample_start: u32,
    pub sample_end: u32,
}

/// Fold per-unit durations into one timed span per source word.
///
/// `unit_spans[i]` is where unit `i` came from (`None` for a unit with no source — BOS,
/// EOS, a symbol the tokenizer dropped), and `unit_samples[i]` is how long it is spoken.
/// The unit can be a token or a phoneme byte; this only needs the two to line up.
///
/// Three rules, each earned by watching real text go through:
///
/// - **A `None` unit consumes time but creates no mark.** Leading and trailing silence is
///   real audio and has to advance the cursor, but it belongs to no word. Marks therefore
///   do not tile the audio, and nothing downstream should assume they do.
/// - **Words are merged on OVERLAP after widening, not grouped by equal span.** espeak
///   reports one token as two words often enough that equality alone leaves two marks over
///   one page word — which the wire format rejects as overlapping, correctly.
/// - **Time is taken from the cursor, never from the span.** The character span says which
///   word; only the durations say when. Mixing the two is how a character-linear mapper
///   gets built by accident.
pub fn aggregate_spans(
    unit_spans: &[Option<Span>],
    unit_samples: &[u32],
    source: &[u8],
) -> Vec<TimedSpan> {
    let mut out: Vec<TimedSpan> = Vec::new();
    let mut cursor: u32 = 0;
    for (i, unit) in unit_spans.iter().enumerate() {
        let start = cursor;
        cursor = cursor.saturating_add(unit_samples.get(i).copied().unwrap_or(0));
        let Some(sp) = *unit else { continue };
        let w = word_bounds(source, sp);
        // A span with no word in it is the pause between two words. It gets its time added
        // to the word before it rather than a mark of its own: a highlight over a space
        // means nothing, and leaving the previous word lit through the pause after it is
        // what a reader expects to see.
        if is_blank(source, w) {
            if let Some(last) = out.last_mut() {
                last.sample_end = cursor;
            }
            continue;
        }
        match out.last_mut() {
            Some(last) if w.start < last.span.end => {
                last.span.start = last.span.start.min(w.start);
                last.span.end = last.span.end.max(w.end);
                last.sample_end = cursor;
            }
            _ => out.push(TimedSpan { span: w, sample_start: start, sample_end: cursor }),
        }
    }
    out
}

/// Resolve a byte [`Span`] of the original text to a half-open UTF-16 code-unit range,
/// using the table from [`utf16_offsets`].
///
/// A span covering only *part* of a character resolves to an EMPTY range, because both
/// ends round down to that character's start. That is not a case a real mark reaches — a
/// mark is aggregated over whole source words, so both its ends are character boundaries —
/// and a mark that did arrive with a zero-length character span is rejected outright by
/// `kokoro_protocol::mark_is_valid`. Fails closed either way; recorded here so nobody
/// later reads an empty range as a mapping failure.
pub fn span_to_utf16(span: Span, offsets: &[u32]) -> (u32, u32) {
    let tail = offsets.last().copied().unwrap_or(0);
    let a = offsets.get(span.start as usize).copied().unwrap_or(tail);
    let b = offsets.get(span.end as usize).copied().unwrap_or(tail);
    (a, b.max(a))
}

// ---- number/currency/decimal expanders (o, c, g) ----------------------------
fn is_pure_number(s: &[u8]) -> bool {
    let mut dot = false;
    let mut digit = false;
    for &c in s {
        if c == b'.' { if dot { return false; } dot = true; }
        else if is_digit(c) { digit = true; }
        else { return false; }
    }
    digit || s.is_empty()
}

fn expand_number_time(e: &[u8]) -> Vec<u8> {
    // o(e)
    if e.contains(&b'.') { return e.to_vec(); }
    if let Some(colon) = e.iter().position(|&c| c == b':') {
        let a = atoi(&e[..colon]);
        let t = atoi(&e[colon + 1..]);
        if t == 0 { return [itoa(a), b" o'clock".to_vec()].concat(); }
        if t < 10 { return [itoa(a), b" oh ".to_vec(), itoa(t)].concat(); }
        return [itoa(a), b" ".to_vec(), itoa(t)].concat();
    }
    let end4 = e.len().min(4);
    let a = atoi(&e[..end4]);
    if a < 1100 || a % 1000 < 10 { return e.to_vec(); }
    let t = e[..2].to_vec();
    let r = atoi(&e[2..e.len().min(4)]);
    let n: &[u8] = if !e.is_empty() && *e.last().unwrap() == b's' { b"s" } else { b"" };
    let m = a % 1000;
    if (100..=999).contains(&m) {
        if r == 0 { return [t, b" hundred".to_vec(), n.to_vec()].concat(); }
        if r < 10 { return [t, b" oh ".to_vec(), itoa(r), n.to_vec()].concat(); }
    }
    [t, b" ".to_vec(), itoa(r), n.to_vec()].concat()
}

fn expand_currency(e: &[u8]) -> Vec<u8> {
    let unit: &[u8] = if e[0] == b'$' { b"dollar" } else { b"pound" };
    let rest = &e[1..];
    if !is_pure_number(rest) {
        return [rest.to_vec(), b" ".to_vec(), unit.to_vec(), b"s".to_vec()].concat();
    }
    if !rest.contains(&b'.') {
        let suf: &[u8] = if rest == b"1" { b"" } else { b"s" };
        return [rest.to_vec(), b" ".to_vec(), unit.to_vec(), suf.to_vec()].concat();
    }
    let dot = rest.iter().position(|&c| c == b'.').unwrap();
    let t = &rest[..dot];
    let mut r = rest[dot + 1..].to_vec();
    while r.len() < 2 { r.push(b'0'); }
    let n = atoi(&r);
    let unit_pl: &[u8] = if t == b"1" { b"" } else { b"s" };
    let cents: &[u8] = if e[0] == b'$' {
        if n == 1 { b"cent" } else { b"cents" }
    } else if n == 1 { b"penny" } else { b"pence" };
    [t.to_vec(), b" ".to_vec(), unit.to_vec(), unit_pl.to_vec(), b" and ".to_vec(),
     itoa(n), b" ".to_vec(), cents.to_vec()].concat()
}

fn expand_decimal(e: &[u8]) -> Vec<u8> {
    let dot = e.iter().position(|&c| c == b'.').unwrap();
    let a = &e[..dot];
    let t = &e[dot + 1..];
    let mut spaced: Vec<u8> = Vec::new();
    for &c in t {
        if !spaced.is_empty() { spaced.push(b' '); }
        spaced.push(c);
    }
    [a.to_vec(), b" point ".to_vec(), spaced].concat()
}

// ---- Stage 1 passes ---------------------------------------------------------
fn pass_numbers(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        let c = s[i];
        if is_digit(c) {
            let mut j = i;
            while j < n && is_digit(s[j]) { j += 1; }
            let len = j - i;
            let boundary_l = i == 0 || !is_word(s[i - 1]);
            // time H:MM or HH:MM
            if boundary_l && (len == 1 || len == 2) && j < n && s[j] == b':' && (i == 0 || s[i - 1] != b':') {
                let hour = atoi(&s[i..i + len]);
                if (1..=12).contains(&hour) && j + 1 < n && is_digit(s[j + 1]) && j + 2 < n && is_digit(s[j + 2]) {
                    let mn = atoi(&s[j + 1..j + 3]);
                    let after = j + 3;
                    let boundary_r = after >= n || !is_word(s[after]);
                    let not_colon = after >= n || s[after] != b':';
                    if (0..=59).contains(&mn) && boundary_r && not_colon {
                        out.emit(&expand_number_time(&s[i..after]), i, after);
                        i = after;
                        continue;
                    }
                }
            }
            // 4-digit year with optional trailing 's'
            if boundary_l && len == 4 {
                let end = j;
                let has_s = end < n && s[end] == b's';
                let wend = if has_s { end + 1 } else { end };
                let boundary_r = wend >= n || !is_word(s[wend]);
                if boundary_r {
                    out.emit(&expand_number_time(&s[i..wend]), i, wend);
                    i = wend;
                    continue;
                }
            }
            out.copy(s, i, i + len);
            i = j;
            continue;
        }
        out.keep(c, i);
        i += 1;
    }
    out.into_parts()
}

fn pass_strip_thousands(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    for i in 0..s.len() {
        if s[i] == b',' && i > 0 && is_digit(s[i - 1]) && i + 1 < s.len() && is_digit(s[i + 1]) {
            continue; // dropped; the digits either side keep their own spans, and any
                      // later expansion of them spans across the gap it leaves
        }
        out.keep(s[i], i);
    }
    out.into_parts()
}

fn is_currency_start(s: &[u8], i: usize) -> bool {
    if s[i] == b'$' { return true; }
    s[i] == 0xC2 && i + 1 < s.len() && s[i + 1] == 0xA3 // £
}

fn pass_currency(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    const SCALES: [&[u8]; 5] = [b" hundred", b" thousand", b" billion", b" million", b" trillion"];
    let mut out = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        if is_currency_start(s, i) {
            let sym_len = if s[i] == b'$' { 1 } else { 2 };
            let mut k = i + sym_len;
            let ds = k;
            while k < n && is_digit(s[k]) { k += 1; }
            if k > ds {
                if k < n && s[k] == b'.' {
                    let mut d2 = k + 1;
                    while d2 < n && is_digit(s[d2]) { d2 += 1; }
                    if d2 > k + 1 { k = d2; }
                }
                loop {
                    let mut matched = false;
                    for sc in SCALES {
                        if eq_at(s, k, sc) { k += sc.len(); matched = true; break; }
                    }
                    if !matched { break; }
                }
                // ExpandCurrency keys on e[0]=='$'; pass '#' sentinel for pound.
                let digits = &s[i + sym_len..k];
                let arg: Vec<u8> = if s[i] == b'$' {
                    [b"$".as_ref(), digits].concat()
                } else {
                    [b"#".as_ref(), digits].concat()
                };
                out.emit(&expand_currency(&arg), i, k);
                i = k;
                continue;
            }
        }
        out.keep(s[i], i);
        i += 1;
    }
    out.into_parts()
}

fn pass_decimals(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        if is_digit(s[i]) || (s[i] == b'.' && i + 1 < n && is_digit(s[i + 1])) {
            let mut j = i;
            while j < n && is_digit(s[j]) { j += 1; }
            if j < n && s[j] == b'.' && j + 1 < n && is_digit(s[j + 1]) {
                let mut d2 = j + 1;
                while d2 < n && is_digit(s[d2]) { d2 += 1; }
                out.emit(&expand_decimal(&s[i..d2]), i, d2);
                i = d2;
                continue;
            }
            out.copy(s, i, j);
            i = j;
            continue;
        }
        out.keep(s[i], i);
        i += 1;
    }
    out.into_parts()
}

fn pass_ranges(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    for i in 0..s.len() {
        if s[i] == b'-' && i > 0 && is_digit(s[i - 1]) && i + 1 < s.len() && is_digit(s[i + 1]) {
            out.emit(b" to ", i, i + 1);
        } else {
            out.keep(s[i], i);
        }
    }
    out.into_parts()
}

fn is_consonant_cap(c: u8) -> bool {
    is_upper(c) && c != b'A' && c != b'E' && c != b'I' && c != b'O' && c != b'U'
}

fn pass_possessive(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        let c = s[i];
        // (?<=X')S\b -> "s"
        if c == b'S' && i >= 2 && s[i - 1] == b'\'' && s[i - 2] == b'X' {
            let b_r = i + 1 >= n || !is_word(s[i + 1]);
            if b_r { out.keep(b's', i); i += 1; continue; }
        }
        // (?<=[consonantCap])'?s\b -> "'S"
        if c == b'\'' && i + 1 < n && s[i + 1] == b's' {
            let b_r = i + 2 >= n || !is_word(s[i + 2]);
            if b_r && !out.is_empty() && is_consonant_cap(out.last().unwrap()) {
                out.emit(b"'S", i, i + 2); i += 2; continue;
            }
        }
        if c == b's' {
            let b_r = i + 1 >= n || !is_word(s[i + 1]);
            if b_r && !out.is_empty() && is_consonant_cap(out.last().unwrap()) {
                out.emit(b"'S", i, i + 1); i += 1; continue;
            }
        }
        // (?<=\d)S -> " S"
        if c == b'S' && !out.is_empty() && is_digit(out.last().unwrap()) {
            out.emit(b" S", i, i + 1); i += 1; continue;
        }
        out.keep(c, i);
        i += 1;
    }
    out.into_parts()
}

fn pass_acronyms(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    // rule 1: (?:[A-Za-z]\.){2,} followed by " " + [a-z]
    let mut a = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        let mut j = i;
        let mut count = 0;
        while j + 1 < n && is_alpha(s[j]) && s[j + 1] == b'.' { j += 2; count += 1; }
        if count >= 2 && j + 1 < n && s[j] == b' ' && (b'a'..=b'z').contains(&s[j + 1]) {
            for k in i..j { a.keep(if s[k] == b'.' { b'-' } else { s[k] }, k); }
            i = j;
            continue;
        }
        a.keep(s[i], i);
        i += 1;
    }
    let (a, a_map) = a.into_parts();
    // rule 2: letter '.' letter -> letter '-' letter
    let mut b = Tracked::new(&a_map);
    let m = a.len();
    for i in 0..m {
        if a[i] == b'.' && i > 0 && is_alpha(a[i - 1]) && i + 1 < m && is_alpha(a[i + 1]) {
            b.keep(b'-', i);
        } else {
            b.keep(a[i], i);
        }
    }
    b.into_parts()
}

fn pass_whitespace(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut a = Tracked::new(src);
    for (i, &c) in s.iter().enumerate() {
        if c == b'\t' || c == b'\r' || c == 0x0B || c == 0x0C { a.keep(b' ', i); } else { a.keep(c, i); }
    }
    let (a, a_map) = a.into_parts();
    // collapse 2+ spaces
    let mut b = Tracked::new(&a_map);
    for i in 0..a.len() {
        if a[i] == b' ' && b.last() == Some(b' ') { continue; }
        b.keep(a[i], i);
    }
    let (b, b_map) = b.into_parts();
    // spaces between newlines
    let mut c = Tracked::new(&b_map);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' ' {
            let prev_nl = c.last() == Some(b'\n');
            let mut k = i;
            while k < b.len() && b[k] == b' ' { k += 1; }
            let next_nl = k < b.len() && b[k] == b'\n';
            if prev_nl && next_nl { i = k; continue; }
        }
        c.keep(b[i], i);
        i += 1;
    }
    c.into_parts()
}

fn pass_titles(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    let n = s.len();
    let wb_l = |i: usize| i == 0 || !is_word(s[i - 1]);
    let mut i = 0;
    while i < n {
        // Dr. / DR. -> Doctor (before " [A-Z]")
        if wb_l(i) && s[i] == b'D' && i + 2 < n && (s[i + 1] == b'r' || s[i + 1] == b'R') && s[i + 2] == b'.'
            && i + 4 < n && s[i + 3] == b' ' && is_upper(s[i + 4]) {
            out.emit(b"Doctor", i, i + 3); i += 3; continue;
        }
        // title helper (Mr./MR. etc.)
        let mut done = false;
        for &(mixed, caps, to) in &[
            (b"Mr.".as_ref(), b"MR.".as_ref(), b"Mister".as_ref()),
            (b"Ms.".as_ref(), b"MS.".as_ref(), b"Miss".as_ref()),
            (b"Mrs.".as_ref(), b"MRS.".as_ref(), b"Mrs".as_ref()),
        ] {
            let l = mixed.len();
            if wb_l(i) && eq_at(s, i, mixed) {
                out.emit(to, i, i + l); i += l; done = true; break;
            }
            if wb_l(i) && eq_at(s, i, caps) && i + l + 1 < n && s[i + l] == b' ' && is_upper(s[i + l + 1]) {
                out.emit(to, i, i + l); i += l; done = true; break;
            }
        }
        if done { continue; }
        // etc. -> etc (case-insensitive, NOT before " [A-Z]")
        if wb_l(i) && lower(s[i]) == b'e' && i + 3 < n && lower(s[i + 1]) == b't' && lower(s[i + 2]) == b'c' && s[i + 3] == b'.' {
            let not_cap_next = !(i + 5 < n && s[i + 4] == b' ' && is_upper(s[i + 5]));
            if not_cap_next { out.emit(&s[i..i + 3], i, i + 4); i += 4; continue; }
        }
        out.keep(s[i], i);
        i += 1;
    }
    out.into_parts()
}

fn pass_yeah(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut out = Tracked::new(src);
    let n = s.len();
    let mut i = 0;
    while i < n {
        let wb_l = i == 0 || !is_word(s[i - 1]);
        if wb_l && lower(s[i]) == b'y' && i + 2 < n && lower(s[i + 1]) == b'e' && lower(s[i + 2]) == b'a' {
            let mut end = i + 3;
            if end < n && lower(s[end]) == b'h' { end += 1; }
            let wb_r = end >= n || !is_word(s[end]);
            if wb_r {
                // "yeah"/"yea" -> "ye'a", keeping the original's leading case.
                let rep = [s[i], b'e', b'\'', b'a'];
                out.emit(&rep, i, end);
                i = end;
                continue;
            }
        }
        out.keep(s[i], i);
        i += 1;
    }
    out.into_parts()
}

fn pass_quotes(s: &[u8], src: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut t = s.to_vec();
    let mut m = src.to_vec();
    // Order matters and is load-bearing: « becomes “ becomes ", and only then does (
    // become «. A single-scan rewrite would apply these simultaneously and change the
    // output, so each stays its own sequential replace.
    let rules: [(&[u8], &[u8]); 15] = [
        (b"\xE2\x80\x98", b"'"),                 // ‘
        (b"\xE2\x80\x99", b"'"),                 // ’
        (b"\xC2\xAB", b"\xE2\x80\x9C"),          // « -> “
        (b"\xC2\xBB", b"\xE2\x80\x9D"),          // » -> ”
        (b"\xE2\x80\x9C", b"\""),                // “ -> "
        (b"\xE2\x80\x9D", b"\""),                // ” -> "
        (b"(", b"\xC2\xAB"),                     // ( -> «
        (b")", b"\xC2\xBB"),                     // ) -> »
        (b"\xE3\x80\x81", b", "),                // 、
        (b"\xE3\x80\x82", b". "),                // 。
        (b"\xEF\xBC\x81", b"! "),                // ！
        (b"\xEF\xBC\x8C", b", "),                // ，
        (b"\xEF\xBC\x9A", b": "),                // ：
        (b"\xEF\xBC\x9B", b"; "),                // ；
        (b"\xEF\xBC\x9F", b"? "),                // ？
    ];
    for (from, to) in rules {
        replace_all_spans(&mut t, &mut m, from, to);
    }
    (t, m)
}

/// Normalized text plus, for every byte of it, the byte range of the ORIGINAL input that
/// produced it. See [`Span`]; [`utf16_offsets`] converts those byte ranges to the UTF-16
/// positions a SAPI mark has to speak in.
pub struct Normalized {
    pub text: Vec<u8>,
    pub spans: Vec<Span>,
}

pub fn normalize_spans(utf8: &[u8]) -> Normalized {
    let m = identity_spans(utf8.len());
    let (s, m) = pass_quotes(utf8, &m);
    let (s, m) = pass_whitespace(&s, &m);
    let (s, m) = pass_titles(&s, &m);
    let (s, m) = pass_yeah(&s, &m);
    let (s, m) = pass_numbers(&s, &m);
    let (s, m) = pass_strip_thousands(&s, &m);
    let (s, m) = pass_currency(&s, &m);
    let (s, m) = pass_decimals(&s, &m);
    let (s, m) = pass_ranges(&s, &m);
    let (s, m) = pass_possessive(&s, &m);
    let (s, m) = pass_acronyms(&s, &m);
    let (text, spans) = trim_spans(&s, &m);
    Normalized { text, spans }
}

// Unused inside the host now that the span-carrying variants have consumers, but NOT
// dead: `kokoro-bench` includes this file with `#[path]` and calls it (kokoro-host is
// bin-only, so there is no lib target to share instead).
#[allow(dead_code)]
/// (Kept for `kokoro-bench`, which includes this file via `#[path]`; the host itself goes
/// through [`normalize_spans`].)
pub fn normalize(utf8: &[u8]) -> Vec<u8> {
    normalize_spans(utf8).text
}

// ---- segmentation -----------------------------------------------------------
pub struct Segment {
    pub is_punct: bool,
    pub text: Vec<u8>,
    /// Byte offset of this segment in the text handed to [`split_segments`]. Segments are
    /// pure slices — nothing is added, dropped or rewritten — so `start .. start +
    /// text.len()` indexes that text, and the same range indexes its span map. That is
    /// what lets a phonemized segment's spans be lifted back to the original source.
    pub start: usize,
}

pub fn split_segments(s: &[u8]) -> Vec<Segment> {
    const PUNCT: [&[u8]; 21] = [
        b";", b":", b",", b".", b"!", b"?", b"\"",
        b"\xC2\xA1", b"\xC2\xBF", // ¡ ¿
        b"\xE2\x80\x94", b"\xE2\x80\xA6", // — …
        b"\xC2\xAB", b"\xC2\xBB", // « »
        b"\xE2\x80\x9C", b"\xE2\x80\x9D", // “ ”
        b"(", b")", b"{", b"}", b"[", b"]",
    ];
    let punct_len_at = |i: usize| -> usize {
        for p in PUNCT { if eq_at(s, i, p) { return p.len(); } }
        0
    };
    let mut segs: Vec<Segment> = Vec::new();
    let n = s.len();
    let mut i = 0;
    let mut text_start = 0;
    while i < n {
        let mut j = i;
        while j < n && s[j] == b' ' { j += 1; }
        if j < n && punct_len_at(j) > 0 {
            let mut run_end = i;
            let mut k = i;
            let mut any = false;
            loop {
                let mut m = k;
                while m < n && s[m] == b' ' { m += 1; }
                let pl = if m < n { punct_len_at(m) } else { 0 };
                if pl == 0 { break; }
                while m < n {
                    let q = punct_len_at(m);
                    if q == 0 { break; }
                    m += q;
                }
                while m < n && s[m] == b' ' { m += 1; }
                k = m;
                any = true;
                run_end = m;
            }
            if any {
                if i > text_start {
                    segs.push(Segment {
                        is_punct: false,
                        text: s[text_start..i].to_vec(),
                        start: text_start,
                    });
                }
                segs.push(Segment { is_punct: true, text: s[i..run_end].to_vec(), start: i });
                i = run_end;
                text_start = i;
                continue;
            }
        }
        i += utf8_len(s[i]);
    }
    if text_start < n {
        segs.push(Segment { is_punct: false, text: s[text_start..].to_vec(), start: text_start });
    }
    segs
}

// ---- phoneme post-processing ------------------------------------------------

/// Phoneme post-processing, carrying a span map through every substitution.
///
/// `spans` maps each input phoneme byte to a source range; the result maps each *output*
/// byte to the same coordinate space. Symbols and spans have to move together here — a
/// symbol that is replaced, joined or dropped must make the identical transformation to
/// its mapping, or every duration after it is aggregated onto the wrong word.
pub fn post_process_spans(phon: &[u8], spans: &[Span]) -> (Vec<u8>, Vec<Span>) {
    let mut s = phon.to_vec();
    let mut m = spans.to_vec();
    let subs: [(&[u8], &[u8]); 6] = [
        // kəkˈoːɹoʊ -> kˈoʊkəɹoʊ
        (b"k\xC9\x99k\xCB\x88o\xCB\x90\xC9\xB9o\xCA\x8A",
         b"k\xCB\x88o\xCA\x8Ak\xC9\x99\xC9\xB9o\xCA\x8A"),
        // kəkˈɔːɹəʊ -> kˈəʊkəɹəʊ
        (b"k\xC9\x99k\xCB\x88\xC9\x94\xCB\x90\xC9\xB9\xC9\x99\xCA\x8A",
         b"k\xCB\x88\xC9\x99\xCA\x8Ak\xC9\x99\xC9\xB9\xC9\x99\xCA\x8A"),
        (b"\xCA\xB2", b"j"),        // ʲ -> j
        (b"r", b"\xC9\xB9"),        // r -> ɹ
        (b"x", b"k"),               // x -> k
        (b"\xC9\xAC", b"l"),        // ɬ -> l
    ];
    for (from, to) in subs {
        replace_all_spans(&mut s, &mut m, from, to);
    }

    // insert space before "hundred" after [a-zɹː]
    {
        let hundred: &[u8] = b"h\xCB\x88\xCA\x8Cnd\xC9\xB9\xC9\xAAd"; // hˈʌndɹɪd
        let mut out = Tracked::new(&m);
        let n = s.len();
        let mut i = 0;
        while i < n {
            if eq_at(&s, i, hundred) && i > 0 {
                let pc = s[i - 1];
                let prev_low = (b'a'..=b'z').contains(&pc);
                let prev_rp = i >= 2 && s[i - 2] == 0xC9 && s[i - 1] == 0xB9; // ɹ
                let prev_len = i >= 2 && s[i - 2] == 0xCB && s[i - 1] == 0x90; // ː
                // The separator belongs to the word it precedes, so it takes that word's
                // span rather than the one it was appended after.
                if prev_low || prev_rp || prev_len { out.keep(b' ', i); }
            }
            out.keep(s[i], i);
            i += 1;
        }
        let parts = out.into_parts();
        s = parts.0;
        m = parts.1;
    }
    // " z" before terminal punctuation/space/end -> "z"
    {
        const ENDERS: [&[u8]; 14] = [
            b";", b":", b",", b".", b"!", b"?", b"\"",
            b"\xC2\xA1", b"\xC2\xBF", b"\xE2\x80\x94", b"\xE2\x80\xA6",
            b"\xC2\xAB", b"\xC2\xBB", b"\xE2\x80\x9C",
        ];
        let mut out = Tracked::new(&m);
        let n = s.len();
        let mut i = 0;
        while i < n {
            if s[i] == b' ' && i + 1 < n && s[i + 1] == b'z' {
                let after = i + 2;
                let mut term = after >= n || s[after] == b' ';
                for e in ENDERS { if eq_at(&s, after, e) { term = true; break; } }
                // The joined `z` covers the dropped space too: it is the same word now.
                if term { out.emit(b"z", i, i + 2); i += 2; continue; }
            }
            out.keep(s[i], i);
            i += 1;
        }
        let parts = out.into_parts();
        s = parts.0;
        m = parts.1;
    }
    // en-us: (?<=nˈaɪn)ti(?!ː) -> "di"
    {
        let nine: &[u8] = b"n\xCB\x88a\xC9\xAAn"; // nˈaɪn
        let mut out = Tracked::new(&m);
        let n = s.len();
        let mut i = 0;
        while i < n {
            if eq_at(&s, i, b"ti") && out.ends_with(nine) {
                let next_len = i + 3 < n && s[i + 2] == 0xCB && s[i + 3] == 0x90; // ː
                if !next_len { out.emit(b"di", i, i + 2); i += 2; continue; }
            }
            out.keep(s[i], i);
            i += 1;
        }
        let parts = out.into_parts();
        s = parts.0;
        m = parts.1;
    }
    trim_spans(&s, &m)
}

// Unused inside the host now that the span-carrying variants have consumers, but NOT
// dead: `kokoro-bench` includes this file with `#[path]` and calls it (kokoro-host is
// bin-only, so there is no lib target to share instead).
#[allow(dead_code)]
/// (Kept for `kokoro-bench`, which includes this file via `#[path]`; the host itself goes
/// through [`post_process_spans`].)
pub fn post_process(phon: &[u8]) -> Vec<u8> {
    post_process_spans(phon, &identity_spans(phon.len())).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> String {
        String::from_utf8(normalize(s.as_bytes())).unwrap()
    }
    fn post(s: &str) -> String {
        String::from_utf8(post_process(s.as_bytes())).unwrap()
    }

    // Golden characterization tests for the Kokoro-js normalization port. The expected
    // values are the outputs that were proven token-identical to the reference C++ /
    // kokoro-js pipeline (max|diff| 0.0 end-to-end); they guard against regressions in
    // the byte-scanning passes.

    #[test]
    fn years_and_times() {
        assert_eq!(norm("In 1977."), "In 19 77.");
        assert_eq!(norm("at 3:45 sharp"), "at 3 45 sharp");
        assert_eq!(norm("at 12:00 noon"), "at 12 o'clock noon");
        assert_eq!(norm("the 1990s era"), "the 19 90s era");
    }

    #[test]
    fn currency() {
        assert_eq!(norm("It costs $3.50."), "It costs 3 dollars and 50 cents.");
        assert_eq!(norm("just $1 flat"), "just 1 dollar flat");
        assert_eq!(norm("about \u{00A3}5.99 each"), "about 5 pounds and 99 pence each");
        assert_eq!(norm("only \u{00A3}1 here"), "only 1 pound here");
    }

    #[test]
    fn decimals_ranges_thousands() {
        assert_eq!(norm("pi is 3.14 today"), "pi is 3 point 1 4 today");
        assert_eq!(norm("Pages 10-20."), "Pages 10 to 20.");
        assert_eq!(norm("that is $1,250 total"), "that is 1250 dollars total");
    }

    #[test]
    fn titles() {
        assert_eq!(norm("Dr. Chen"), "Doctor Chen");
        assert_eq!(norm("Mr. Smith and Mrs. Jones"), "Mister Smith and Mrs Jones");
        assert_eq!(norm("Ms. Lee, etc."), "Miss Lee, etc");
    }

    #[test]
    fn possessives_and_plurals() {
        assert_eq!(norm("IBM's plan"), "IBM'S plan");
        assert_eq!(norm("James's book"), "James's book"); // lowercase-s: untouched
    }

    #[test]
    fn quotes_and_parens() {
        assert_eq!(norm("(hi)"), "\u{00AB}hi\u{00BB}"); // ( ) -> guillemets
        assert_eq!(norm("\u{201C}quoted\u{201D}"), "\"quoted\""); // curly -> straight
    }

    #[test]
    fn post_process_substitutions() {
        assert_eq!(post("worried"), "wo\u{0279}\u{0279}ied"); // r -> ɹ
        assert_eq!(post("box"), "bok"); // x -> k
        // ti -> di after nˈaɪn ("ninety")
        assert_eq!(post("n\u{02C8}a\u{026A}nti"), "n\u{02C8}a\u{026A}ndi");
    }

    #[test]
    fn idempotent_plain_text() {
        // Text with nothing to normalize passes through unchanged (after trim).
        assert_eq!(norm("hello world"), "hello world");
        assert_eq!(norm("  spaced   out  "), "spaced out");
    }

    // ---- source spans -------------------------------------------------------
    // These are the deterministic half of Task 1: a word mark can only be as good as the
    // mapping from a spoken sound back to the source token, and that mapping starts here.

    /// The stretch of ORIGINAL text that `needle` (a substring of the normalized output)
    /// traces back to.
    fn source_of(orig: &str, needle: &str) -> String {
        let n = normalize_spans(orig.as_bytes());
        let text = String::from_utf8(n.text.clone()).unwrap();
        let at = text.find(needle).unwrap_or_else(|| panic!("{needle:?} not in {text:?}"));
        let lo = n.spans[at].start as usize;
        let hi = n.spans[at + needle.len() - 1].end as usize;
        String::from_utf8_lossy(&orig.as_bytes()[lo..hi]).into_owned()
    }

    /// Every text a page of a book can plausibly contain, so the structural invariants
    /// below are checked against expansions, drops, joins and multi-byte characters
    /// rather than only against prose that happens to pass straight through.
    const CORPUS: &[&str] = &[
        "hello world",
        "  spaced   out  ",
        "In 1977 the price was $3.50, up from \u{00A3}1.",
        "Dr. Chen and Mr. Smith met at 3:45, etc.",
        "Pages 10-20, that is 1,250 words \u{2014} pi is 3.14.",
        "IBM's plan (revised) said \u{201C}yeah\u{201D} at 12:00 noon.",
        "The 1990s were, on balance, fine; nobody argued.",
        "caf\u{00E9} na\u{00EF}ve r\u{00E9}sum\u{00E9} \u{2014} \u{4F60}\u{597D}\u{3002}",
        "",
        "   ",
    ];

    #[test]
    fn every_normalized_byte_has_a_span() {
        for &t in CORPUS {
            let n = normalize_spans(t.as_bytes());
            assert_eq!(n.text.len(), n.spans.len(), "{t:?}");
        }
    }

    #[test]
    fn spans_are_monotonic_and_in_bounds() {
        for &t in CORPUS {
            let n = normalize_spans(t.as_bytes());
            let end = t.len() as u32;
            let mut prev = Span { start: 0, end: 0 };
            for (i, sp) in n.spans.iter().enumerate() {
                assert!(sp.start <= sp.end, "{t:?} byte {i}: reversed {sp:?}");
                assert!(sp.end <= end, "{t:?} byte {i}: {sp:?} past {end}");
                assert!(sp.start >= prev.start, "{t:?} byte {i}: start went back");
                assert!(sp.end >= prev.end, "{t:?} byte {i}: end went back");
                prev = *sp;
            }
        }
    }

    #[test]
    fn plain_text_maps_byte_for_byte() {
        let n = normalize_spans(b"hello world");
        for (i, sp) in n.spans.iter().enumerate() {
            assert_eq!(*sp, Span { start: i as u32, end: i as u32 + 1 });
        }
    }

    #[test]
    fn trimming_shifts_spans_off_the_leading_whitespace() {
        let n = normalize_spans(b"   hi  ");
        assert_eq!(n.text, b"hi");
        assert_eq!(n.spans[0], Span { start: 3, end: 4 });
    }

    #[test]
    fn an_expansion_keeps_its_whole_source_token() {
        // "1977" -> "19 77": all five output bytes point back at the four-byte source
        // token, so the highlight covers "1977" for as long as it is being spoken.
        assert_eq!(source_of("In 1977.", "19 77"), "1977");
        assert_eq!(source_of("In 1977.", "19"), "1977");
        assert_eq!(source_of("In 1977.", "77"), "1977");
        // and the untouched text around it still maps to itself
        assert_eq!(source_of("In 1977.", "In"), "In");
    }

    #[test]
    fn currency_and_time_expansions_keep_their_source_token() {
        assert_eq!(source_of("It costs $3.50.", "3 dollars and 50 cents"), "$3.50");
        assert_eq!(source_of("It costs $3.50.", "cents"), "$3.50");
        assert_eq!(source_of("at 12:00 noon", "12 o'clock"), "12:00");
        assert_eq!(source_of("about \u{00A3}5.99 each", "5 pounds and 99 pence"), "\u{00A3}5.99");
    }

    #[test]
    fn an_expansion_spans_across_a_dropped_separator() {
        // The thousands comma is dropped, then "1250" expands. The span has to cover the
        // comma the expansion no longer contains, or the highlight stops short of it.
        assert_eq!(source_of("that is $1,250 total", "1250 dollars"), "$1,250");
    }

    #[test]
    fn titles_and_ranges_keep_their_source_token() {
        assert_eq!(source_of("Dr. Chen", "Doctor"), "Dr.");
        assert_eq!(source_of("Mr. Smith", "Mister"), "Mr.");
        assert_eq!(source_of("Ms. Lee, etc.", "etc"), "etc.");
        assert_eq!(source_of("Pages 10-20.", " to "), "-");
    }

    #[test]
    fn quote_substitution_keeps_its_source_character() {
        // A curly quote is 3 bytes and becomes 1; a paren is 1 byte and becomes 2.
        assert_eq!(source_of("\u{201C}quoted\u{201D}", "\""), "\u{201C}");
        assert_eq!(source_of("(hi)", "\u{00AB}"), "(");
    }

    #[test]
    fn post_process_moves_spans_with_its_symbols() {
        // r -> ɹ is one byte becoming two; both must still point at the source `r`.
        let src = b"worried";
        let (out, spans) = post_process_spans(src, &identity_spans(src.len()));
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "wo\u{0279}\u{0279}ied");
        assert_eq!(out.len(), spans.len());
        assert_eq!(spans[2], Span { start: 2, end: 3 }); // first ɹ, both bytes
        assert_eq!(spans[3], Span { start: 2, end: 3 });
        assert_eq!(spans[4], Span { start: 3, end: 4 }); // second ɹ
        let mut prev = 0;
        for sp in &spans {
            assert!(sp.start >= prev);
            prev = sp.start;
        }
    }

    #[test]
    fn post_process_join_covers_the_dropped_space() {
        // " z" before a terminator joins into "z"; the surviving byte owns both.
        let src = b"kat z.";
        let (out, spans) = post_process_spans(src, &identity_spans(src.len()));
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "katz.");
        assert_eq!(spans[3], Span { start: 3, end: 5 });
    }

    #[test]
    fn lift_resolves_a_slice_local_span() {
        // "one, two" normalized; a span local to the segment starting at byte 5.
        let n = normalize_spans(b"one, two");
        assert_eq!(lift(&n.spans, 5, Span { start: 0, end: 3 }), Some(Span { start: 5, end: 8 }));
        assert_eq!(lift(&n.spans, 5, Span { start: 1, end: 2 }), Some(Span { start: 6, end: 7 }));
        // Past the end of the map: None, so the caller chooses the coarser answer.
        assert_eq!(lift(&n.spans, 5, Span { start: 0, end: 99 }), None);
        assert_eq!(lift(&n.spans, 99, Span { start: 0, end: 1 }), None);
        // An empty local span resolves to an empty span at that offset rather than to
        // None — it is a position, not a failure. `snap_to_chars` later widens it to the
        // character it sits in, so it cannot become a zero-length mark.
        assert_eq!(lift(&n.spans, 5, Span { start: 0, end: 0 }), Some(Span { start: 5, end: 5 }));
    }

    #[test]
    fn snap_to_chars_keeps_a_character_whole() {
        let s = "a\u{2014}b".as_bytes(); // a, em-dash (3 bytes), b
        // Any span touching part of the dash widens to all of it.
        assert_eq!(snap_to_chars(Span { start: 1, end: 2 }, s), Span { start: 1, end: 4 });
        assert_eq!(snap_to_chars(Span { start: 2, end: 3 }, s), Span { start: 1, end: 4 });
        assert_eq!(snap_to_chars(Span { start: 2, end: 2 }, s), Span { start: 1, end: 4 });
        // Whole characters are left alone.
        assert_eq!(snap_to_chars(Span { start: 0, end: 1 }, s), Span { start: 0, end: 1 });
        assert_eq!(snap_to_chars(Span { start: 1, end: 4 }, s), Span { start: 1, end: 4 });
        assert_eq!(snap_to_chars(Span { start: 0, end: 5 }, s), Span { start: 0, end: 5 });
        // Out of range clamps rather than panicking.
        assert_eq!(snap_to_chars(Span { start: 9, end: 9 }, s), Span { start: 5, end: 5 });
    }

    #[test]
    fn word_bounds_widens_to_the_page_word() {
        let s = b"a state-of-the-art result";
        // Any part of the compound widens to the whole of it.
        assert_eq!(word_bounds(s, Span { start: 2, end: 7 }), Span { start: 2, end: 18 });
        assert_eq!(word_bounds(s, Span { start: 14, end: 18 }), Span { start: 2, end: 18 });
        // espeak builds word extents by TILING, so a word's span runs up to where the next
        // one starts and carries the separating space. Widening from that end must not
        // swallow the following word — doing so folded a whole line into one mark.
        let p = b"The keeper climbed";
        assert_eq!(word_bounds(p, Span { start: 0, end: 4 }), Span { start: 0, end: 3 });
        assert_eq!(word_bounds(p, Span { start: 4, end: 11 }), Span { start: 4, end: 10 });
        // A span that is nothing but whitespace has no word to widen to.
        assert_eq!(word_bounds(p, Span { start: 3, end: 4 }), Span { start: 3, end: 4 });

        let t = "that is 1,250 words".as_bytes();
        // The two halves espeak reports either side of the dropped comma agree once
        // widened, which is what lets them merge.
        assert_eq!(word_bounds(t, Span { start: 8, end: 9 }), Span { start: 8, end: 13 });
        assert_eq!(word_bounds(t, Span { start: 10, end: 13 }), Span { start: 8, end: 13 });
    }

    #[test]
    fn aggregate_merges_split_reports_of_one_word() {
        let src = "that is 1,250 words".as_bytes();
        //            "1"         "250"       "words"
        let spans = vec![
            None,                                    // BOS: silence
            Some(Span { start: 8, end: 9 }),
            Some(Span { start: 10, end: 13 }),
            Some(Span { start: 14, end: 19 }),
            None,                                    // EOS
        ];
        let samples = vec![100, 300, 500, 400, 50];
        let m = aggregate_spans(&spans, &samples, src);
        assert_eq!(m.len(), 2, "the split report of 1,250 must become one mark");
        assert_eq!(m[0], TimedSpan { span: Span { start: 8, end: 13 }, sample_start: 100, sample_end: 900 });
        assert_eq!(m[1], TimedSpan { span: Span { start: 14, end: 19 }, sample_start: 900, sample_end: 1300 });
    }

    #[test]
    fn aggregate_gives_a_pause_to_the_word_before_it() {
        let src = b"hi there";
        let spans = vec![
            Some(Span { start: 0, end: 2 }),  // "hi"
            Some(Span { start: 2, end: 3 }),  // the space between them
            Some(Span { start: 3, end: 8 }),  // "there"
        ];
        let samples = vec![300, 120, 400];
        let m = aggregate_spans(&spans, &samples, src);
        assert_eq!(m.len(), 2, "a space must not become a mark of its own");
        assert_eq!(m[0].span, Span { start: 0, end: 2 });
        assert_eq!(m[0].sample_end, 420, "the pause belongs to the word before it");
        assert_eq!(m[1].sample_start, 420);
    }

    #[test]
    fn aggregate_gives_silence_time_but_no_mark() {
        let src = b"hi there";
        let spans = vec![None, Some(Span { start: 0, end: 2 }), None];
        let samples = vec![240, 600, 240];
        let m = aggregate_spans(&spans, &samples, src);
        assert_eq!(m.len(), 1);
        // Starts after the leading silence, ends before the trailing silence: marks do not
        // tile the audio.
        assert_eq!(m[0].sample_start, 240);
        assert_eq!(m[0].sample_end, 840);
    }

    #[test]
    fn aggregate_output_satisfies_the_wire_rules() {
        // Whatever it produces must be monotonic and non-overlapping in both coordinates,
        // because kokoro_protocol::mark_is_valid rejects anything else.
        let src = "In 1977 it cost $3.50, up from a lot.".as_bytes();
        let spans: Vec<Option<Span>> = vec![
            None,
            Some(Span { start: 0, end: 2 }),
            Some(Span { start: 3, end: 7 }),
            Some(Span { start: 3, end: 7 }), // the same word reported twice
            Some(Span { start: 4, end: 8 }), // ...and again, overlapping
            Some(Span { start: 16, end: 21 }),
            None,
        ];
        let samples = vec![50, 200, 300, 250, 100, 700, 50];
        let m = aggregate_spans(&spans, &samples, src);
        let mut prev = (0u32, 0u32);
        for t in &m {
            assert!(t.span.start >= prev.0, "character spans overlap: {t:?}");
            assert!(t.sample_start >= prev.1, "sample spans overlap: {t:?}");
            assert!(t.span.start < t.span.end && t.sample_start <= t.sample_end);
            prev = (t.span.end, t.sample_end);
        }
        assert_eq!(m.len(), 3, "1977 reported three times is still one word");
    }

    #[test]
    fn segments_index_back_into_their_input() {
        let s = b"one, two. three";
        for seg in split_segments(s) {
            assert_eq!(&s[seg.start..seg.start + seg.text.len()], &seg.text[..]);
        }
    }

    #[test]
    fn utf16_offsets_count_code_units_not_bytes() {
        // ASCII: one unit per byte.
        assert_eq!(utf16_offsets(b"abc"), vec![0, 1, 2, 3]);
        // £ is 2 bytes / 1 unit; both bytes report the lead byte's offset.
        assert_eq!(utf16_offsets("a\u{00A3}b".as_bytes()), vec![0, 1, 1, 2, 3]);
        // An emoji is 4 bytes / 2 units — a surrogate pair, and SAPI counts both.
        assert_eq!(utf16_offsets("a\u{1F600}b".as_bytes()), vec![0, 1, 1, 1, 1, 3, 4]);
    }

    #[test]
    fn span_to_utf16_resolves_against_the_original() {
        let orig = "a\u{1F600} bc";
        let off = utf16_offsets(orig.as_bytes());
        // the emoji: bytes 1..5 -> units 1..3
        assert_eq!(span_to_utf16(Span { start: 1, end: 5 }, &off), (1, 3));
        // "bc" at the end: bytes 6..8 -> units 4..6
        assert_eq!(span_to_utf16(Span { start: 6, end: 8 }, &off), (4, 6));
        // out of range degenerates to the end rather than panicking
        assert_eq!(span_to_utf16(Span { start: 99, end: 99 }, &off), (6, 6));
        // Part of a character collapses to an empty range rather than half a surrogate
        // pair. A real mark spans whole words, and mark_is_valid rejects char_len == 0.
        assert_eq!(span_to_utf16(Span { start: 2, end: 3 }, &off), (1, 1));
    }
}
