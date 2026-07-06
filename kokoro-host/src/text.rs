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

fn replace_all(s: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    if from.is_empty() { return; }
    let mut p = 0;
    while let Some(idx) = find_from(s, from, p) {
        s.splice(idx..idx + from.len(), to.iter().copied());
        p = idx + to.len();
    }
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
fn pass_numbers(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
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
                        out.extend_from_slice(&expand_number_time(&s[i..after]));
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
                    out.extend_from_slice(&expand_number_time(&s[i..wend]));
                    i = wend;
                    continue;
                }
            }
            out.extend_from_slice(&s[i..i + len]);
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn pass_strip_thousands(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..s.len() {
        if s[i] == b',' && i > 0 && is_digit(s[i - 1]) && i + 1 < s.len() && is_digit(s[i + 1]) {
            continue;
        }
        out.push(s[i]);
    }
    out
}

fn is_currency_start(s: &[u8], i: usize) -> bool {
    if s[i] == b'$' { return true; }
    s[i] == 0xC2 && i + 1 < s.len() && s[i + 1] == 0xA3 // £
}

fn pass_currency(s: &[u8]) -> Vec<u8> {
    const SCALES: [&[u8]; 5] = [b" hundred", b" thousand", b" billion", b" million", b" trillion"];
    let mut out = Vec::new();
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
                out.extend_from_slice(&expand_currency(&arg));
                i = k;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn pass_decimals(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let n = s.len();
    let mut i = 0;
    while i < n {
        if is_digit(s[i]) || (s[i] == b'.' && i + 1 < n && is_digit(s[i + 1])) {
            let mut j = i;
            while j < n && is_digit(s[j]) { j += 1; }
            if j < n && s[j] == b'.' && j + 1 < n && is_digit(s[j + 1]) {
                let mut d2 = j + 1;
                while d2 < n && is_digit(s[d2]) { d2 += 1; }
                out.extend_from_slice(&expand_decimal(&s[i..d2]));
                i = d2;
                continue;
            }
            out.extend_from_slice(&s[i..j]);
            i = j;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn pass_ranges(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..s.len() {
        if s[i] == b'-' && i > 0 && is_digit(s[i - 1]) && i + 1 < s.len() && is_digit(s[i + 1]) {
            out.extend_from_slice(b" to ");
        } else {
            out.push(s[i]);
        }
    }
    out
}

fn is_consonant_cap(c: u8) -> bool {
    is_upper(c) && c != b'A' && c != b'E' && c != b'I' && c != b'O' && c != b'U'
}

fn pass_possessive(s: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let n = s.len();
    let mut i = 0;
    while i < n {
        let c = s[i];
        // (?<=X')S\b -> "s"
        if c == b'S' && i >= 2 && s[i - 1] == b'\'' && s[i - 2] == b'X' {
            let b_r = i + 1 >= n || !is_word(s[i + 1]);
            if b_r { out.push(b's'); i += 1; continue; }
        }
        // (?<=[consonantCap])'?s\b -> "'S"
        if c == b'\'' && i + 1 < n && s[i + 1] == b's' {
            let b_r = i + 2 >= n || !is_word(s[i + 2]);
            if b_r && !out.is_empty() && is_consonant_cap(*out.last().unwrap()) {
                out.extend_from_slice(b"'S"); i += 2; continue;
            }
        }
        if c == b's' {
            let b_r = i + 1 >= n || !is_word(s[i + 1]);
            if b_r && !out.is_empty() && is_consonant_cap(*out.last().unwrap()) {
                out.extend_from_slice(b"'S"); i += 1; continue;
            }
        }
        // (?<=\d)S -> " S"
        if c == b'S' && !out.is_empty() && is_digit(*out.last().unwrap()) {
            out.extend_from_slice(b" S"); i += 1; continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn pass_acronyms(s: &[u8]) -> Vec<u8> {
    // rule 1: (?:[A-Za-z]\.){2,} followed by " " + [a-z]
    let mut a: Vec<u8> = Vec::new();
    let n = s.len();
    let mut i = 0;
    while i < n {
        let mut j = i;
        let mut count = 0;
        while j + 1 < n && is_alpha(s[j]) && s[j + 1] == b'.' { j += 2; count += 1; }
        if count >= 2 && j + 1 < n && s[j] == b' ' && (b'a'..=b'z').contains(&s[j + 1]) {
            for k in i..j { a.push(if s[k] == b'.' { b'-' } else { s[k] }); }
            i = j;
            continue;
        }
        a.push(s[i]);
        i += 1;
    }
    // rule 2: letter '.' letter -> letter '-' letter
    let mut b: Vec<u8> = Vec::new();
    let m = a.len();
    for i in 0..m {
        if a[i] == b'.' && i > 0 && is_alpha(a[i - 1]) && i + 1 < m && is_alpha(a[i + 1]) {
            b.push(b'-');
        } else {
            b.push(a[i]);
        }
    }
    b
}

fn pass_whitespace(s: &[u8]) -> Vec<u8> {
    let mut a: Vec<u8> = Vec::new();
    for &c in s {
        if c == b'\t' || c == b'\r' || c == 0x0B || c == 0x0C { a.push(b' '); } else { a.push(c); }
    }
    // collapse 2+ spaces
    let mut b: Vec<u8> = Vec::new();
    for i in 0..a.len() {
        if a[i] == b' ' && !b.is_empty() && *b.last().unwrap() == b' ' { continue; }
        b.push(a[i]);
    }
    // spaces between newlines
    let mut c: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b' ' {
            let prev_nl = !c.is_empty() && *c.last().unwrap() == b'\n';
            let mut k = i;
            while k < b.len() && b[k] == b' ' { k += 1; }
            let next_nl = k < b.len() && b[k] == b'\n';
            if prev_nl && next_nl { i = k; continue; }
        }
        c.push(b[i]);
        i += 1;
    }
    c
}

fn pass_titles(s: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let n = s.len();
    let wb_l = |i: usize| i == 0 || !is_word(s[i - 1]);
    let mut i = 0;
    while i < n {
        // Dr. / DR. -> Doctor (before " [A-Z]")
        if wb_l(i) && s[i] == b'D' && i + 2 < n && (s[i + 1] == b'r' || s[i + 1] == b'R') && s[i + 2] == b'.'
            && i + 4 < n && s[i + 3] == b' ' && is_upper(s[i + 4]) {
            out.extend_from_slice(b"Doctor"); i += 3; continue;
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
                out.extend_from_slice(to); i += l; done = true; break;
            }
            if wb_l(i) && eq_at(s, i, caps) && i + l + 1 < n && s[i + l] == b' ' && is_upper(s[i + l + 1]) {
                out.extend_from_slice(to); i += l; done = true; break;
            }
        }
        if done { continue; }
        // etc. -> etc (case-insensitive, NOT before " [A-Z]")
        if wb_l(i) && lower(s[i]) == b'e' && i + 3 < n && lower(s[i + 1]) == b't' && lower(s[i + 2]) == b'c' && s[i + 3] == b'.' {
            let not_cap_next = !(i + 5 < n && s[i + 4] == b' ' && is_upper(s[i + 5]));
            if not_cap_next { out.extend_from_slice(&s[i..i + 3]); i += 4; continue; }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn pass_yeah(s: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let n = s.len();
    let mut i = 0;
    while i < n {
        let wb_l = i == 0 || !is_word(s[i - 1]);
        if wb_l && lower(s[i]) == b'y' && i + 2 < n && lower(s[i + 1]) == b'e' && lower(s[i + 2]) == b'a' {
            let mut end = i + 3;
            if end < n && lower(s[end]) == b'h' { end += 1; }
            let wb_r = end >= n || !is_word(s[end]);
            if wb_r { out.push(s[i]); out.extend_from_slice(b"e'a"); i = end; continue; }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn pass_quotes(s: &[u8]) -> Vec<u8> {
    let mut t = s.to_vec();
    replace_all(&mut t, b"\xE2\x80\x98", b"'"); // ‘
    replace_all(&mut t, b"\xE2\x80\x99", b"'"); // ’
    replace_all(&mut t, b"\xC2\xAB", b"\xE2\x80\x9C"); // « -> “
    replace_all(&mut t, b"\xC2\xBB", b"\xE2\x80\x9D"); // » -> ”
    replace_all(&mut t, b"\xE2\x80\x9C", b"\""); // “ -> "
    replace_all(&mut t, b"\xE2\x80\x9D", b"\""); // ” -> "
    replace_all(&mut t, b"(", b"\xC2\xAB"); // ( -> «
    replace_all(&mut t, b")", b"\xC2\xBB"); // ) -> »
    replace_all(&mut t, b"\xE3\x80\x81", b", "); // 、
    replace_all(&mut t, b"\xE3\x80\x82", b". "); // 。
    replace_all(&mut t, b"\xEF\xBC\x81", b"! "); // ！
    replace_all(&mut t, b"\xEF\xBC\x8C", b", "); // ，
    replace_all(&mut t, b"\xEF\xBC\x9A", b": "); // ：
    replace_all(&mut t, b"\xEF\xBC\x9B", b"; "); // ；
    replace_all(&mut t, b"\xEF\xBC\x9F", b"? "); // ？
    t
}

fn trim(s: &[u8]) -> Vec<u8> {
    let mut a = 0;
    let mut b = s.len();
    while a < b && s[a] <= b' ' { a += 1; }
    while b > a && s[b - 1] <= b' ' { b -= 1; }
    s[a..b].to_vec()
}

pub fn normalize(utf8: &[u8]) -> Vec<u8> {
    let s = pass_quotes(utf8);
    let s = pass_whitespace(&s);
    let s = pass_titles(&s);
    let s = pass_yeah(&s);
    let s = pass_numbers(&s);
    let s = pass_strip_thousands(&s);
    let s = pass_currency(&s);
    let s = pass_decimals(&s);
    let s = pass_ranges(&s);
    let s = pass_possessive(&s);
    let s = pass_acronyms(&s);
    trim(&s)
}

// ---- segmentation -----------------------------------------------------------
pub struct Segment {
    pub is_punct: bool,
    pub text: Vec<u8>,
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
                    segs.push(Segment { is_punct: false, text: s[text_start..i].to_vec() });
                }
                segs.push(Segment { is_punct: true, text: s[i..run_end].to_vec() });
                i = run_end;
                text_start = i;
                continue;
            }
        }
        i += utf8_len(s[i]);
    }
    if text_start < n {
        segs.push(Segment { is_punct: false, text: s[text_start..].to_vec() });
    }
    segs
}

// ---- phoneme post-processing ------------------------------------------------
pub fn post_process(phon: &[u8]) -> Vec<u8> {
    let mut s = phon.to_vec();
    // kəkˈoːɹoʊ -> kˈoʊkəɹoʊ
    replace_all(&mut s, b"k\xC9\x99k\xCB\x88o\xCB\x90\xC9\xB9o\xCA\x8A",
                b"k\xCB\x88o\xCA\x8Ak\xC9\x99\xC9\xB9o\xCA\x8A");
    // kəkˈɔːɹəʊ -> kˈəʊkəɹəʊ
    replace_all(&mut s, b"k\xC9\x99k\xCB\x88\xC9\x94\xCB\x90\xC9\xB9\xC9\x99\xCA\x8A",
                b"k\xCB\x88\xC9\x99\xCA\x8Ak\xC9\x99\xC9\xB9\xC9\x99\xCA\x8A");
    replace_all(&mut s, b"\xCA\xB2", b"j"); // ʲ -> j
    replace_all(&mut s, b"r", b"\xC9\xB9"); // r -> ɹ
    replace_all(&mut s, b"x", b"k"); // x -> k
    replace_all(&mut s, b"\xC9\xAC", b"l"); // ɬ -> l

    // insert space before "hundred" after [a-zɹː]
    {
        let hundred: &[u8] = b"h\xCB\x88\xCA\x8Cnd\xC9\xB9\xC9\xAAd"; // hˈʌndɹɪd
        let mut out: Vec<u8> = Vec::new();
        let n = s.len();
        let mut i = 0;
        while i < n {
            if eq_at(&s, i, hundred) && i > 0 {
                let pc = s[i - 1];
                let prev_low = (b'a'..=b'z').contains(&pc);
                let prev_rp = i >= 2 && s[i - 2] == 0xC9 && s[i - 1] == 0xB9; // ɹ
                let prev_len = i >= 2 && s[i - 2] == 0xCB && s[i - 1] == 0x90; // ː
                if prev_low || prev_rp || prev_len { out.push(b' '); }
            }
            out.push(s[i]);
            i += 1;
        }
        s = out;
    }
    // " z" before terminal punctuation/space/end -> "z"
    {
        const ENDERS: [&[u8]; 14] = [
            b";", b":", b",", b".", b"!", b"?", b"\"",
            b"\xC2\xA1", b"\xC2\xBF", b"\xE2\x80\x94", b"\xE2\x80\xA6",
            b"\xC2\xAB", b"\xC2\xBB", b"\xE2\x80\x9C",
        ];
        let mut out: Vec<u8> = Vec::new();
        let n = s.len();
        let mut i = 0;
        while i < n {
            if s[i] == b' ' && i + 1 < n && s[i + 1] == b'z' {
                let after = i + 2;
                let mut term = after >= n || s[after] == b' ';
                for e in ENDERS { if eq_at(&s, after, e) { term = true; break; } }
                if term { out.push(b'z'); i += 2; continue; }
            }
            out.push(s[i]);
            i += 1;
        }
        s = out;
    }
    // en-us: (?<=nˈaɪn)ti(?!ː) -> "di"
    {
        let nine: &[u8] = b"n\xCB\x88a\xC9\xAAn"; // nˈaɪn
        let mut out: Vec<u8> = Vec::new();
        let n = s.len();
        let mut i = 0;
        while i < n {
            if eq_at(&s, i, b"ti") && out.len() >= nine.len() && &out[out.len() - nine.len()..] == nine {
                let next_len = i + 3 < n && s[i + 2] == 0xCB && s[i + 3] == 0x90; // ː
                if !next_len { out.extend_from_slice(b"di"); i += 2; continue; }
            }
            out.push(s[i]);
            i += 1;
        }
        s = out;
    }
    trim(&s)
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
}
