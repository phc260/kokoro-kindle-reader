#include "KokoroText.h"
#include <cctype>
#include <cstdlib>
#include <string>
#include <vector>

// Port of kokoro-js `m()` normalization. See tools/kokoro-phonemize-spec.md for the
// verbatim JS. Works on UTF-8 bytes; the regex passes are hand-scanned because
// std::regex lacks lookbehind. Validated to token-parity by tools/phonemize_test.cpp.
namespace kokoro_text {
namespace {

using std::string;
const size_t npos = string::npos;

// ---- small helpers ----
bool IsDigit(char c) { return c >= '0' && c <= '9'; }
bool IsAlpha(char c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); }
bool IsUpper(char c) { return c >= 'A' && c <= 'Z'; }
bool IsWord(char c) { return IsAlpha(c) || IsDigit(c) || c == '_'; }
char Lower(char c) { return (c >= 'A' && c <= 'Z') ? char(c - 'A' + 'a') : c; }

void ReplaceAll(string& s, const string& from, const string& to) {
  if (from.empty()) return;
  size_t p = 0;
  while ((p = s.find(from, p)) != npos) { s.replace(p, from.size(), to); p += to.size(); }
}

// A UTF-8 codepoint's byte length from its lead byte.
int Utf8Len(unsigned char c) {
  if (c < 0x80) return 1;
  if ((c >> 5) == 0x6) return 2;
  if ((c >> 4) == 0xE) return 3;
  if ((c >> 3) == 0x1E) return 4;
  return 1;
}

// ---- number/currency/decimal expanders (o, c, g) ----
bool IsPureNumber(const string& s) {  // matches JS !isNaN(Number(s)) for our inputs
  bool dot = false, digit = false;
  for (char c : s) {
    if (c == '.') { if (dot) return false; dot = true; }
    else if (IsDigit(c)) digit = true;
    else return false;
  }
  return digit || s.empty();
}

string ExpandNumberTime(const string& e) {  // o(e)
  if (e.find('.') != npos) return e;
  size_t colon = e.find(':');
  if (colon != npos) {
    int a = std::atoi(e.substr(0, colon).c_str());
    int t = std::atoi(e.substr(colon + 1).c_str());
    if (t == 0) return std::to_string(a) + " o'clock";
    if (t < 10) return std::to_string(a) + " oh " + std::to_string(t);
    return std::to_string(a) + " " + std::to_string(t);
  }
  int a = std::atoi(e.substr(0, 4).c_str());
  if (a < 1100 || a % 1000 < 10) return e;
  string t = e.substr(0, 2);
  int r = std::atoi(e.substr(2, 2).c_str());
  string n = (!e.empty() && e.back() == 's') ? "s" : "";
  int mod = a % 1000;
  if (mod >= 100 && mod <= 999) {
    if (r == 0) return t + " hundred" + n;
    if (r < 10) return t + " oh " + std::to_string(r) + n;
  }
  return t + " " + std::to_string(r) + n;
}

string ExpandCurrency(const string& e) {  // c(e)
  string unit = (e[0] == '$') ? "dollar" : "pound";
  string rest = e.substr(1);
  if (!IsPureNumber(rest)) return rest + " " + unit + "s";
  if (rest.find('.') == npos) {
    string suf = (rest == "1") ? "" : "s";
    return rest + " " + unit + suf;
  }
  size_t dot = rest.find('.');
  string t = rest.substr(0, dot), r = rest.substr(dot + 1);
  while (r.size() < 2) r += '0';
  int n = std::atoi(r.c_str());
  string unitPl = (t == "1") ? "" : "s";
  string cents = (e[0] == '$') ? (n == 1 ? "cent" : "cents") : (n == 1 ? "penny" : "pence");
  return t + " " + unit + unitPl + " and " + std::to_string(n) + " " + cents;
}

string ExpandDecimal(const string& e) {  // g(e)
  size_t dot = e.find('.');
  string a = e.substr(0, dot), t = e.substr(dot + 1), spaced;
  for (char c : t) { if (!spaced.empty()) spaced += ' '; spaced += c; }
  return a + " point " + spaced;
}

// ---- Stage 1 passes ----

// abbreviation table for the Mr./Ms./Mrs. family (case-exact "Xr." always matches;
// all-caps "XR." only before " [A-Z]"). Dr. handled separately (D + r/R).
struct Abbr { const char* from; const char* to; };

// Apply the `o` expander to years/times: \d*\.\d+ (passed through unchanged by o),
// \b\d{4}s?\b, and (?<!:)\b(1-12):(00-59)\b(?!:). We only need the year + time forms
// to change; decimals are left for the currency/decimal passes.
string PassNumbers(const string& s) {
  string out;
  size_t i = 0, n = s.size();
  while (i < n) {
    char c = s[i];
    // time  H:MM or HH:MM, hour 1-12, min 00-59, not adjacent to another ':'
    if (IsDigit(c)) {
      // find the run of digits
      size_t j = i;
      while (j < n && IsDigit(s[j])) j++;
      size_t len = j - i;
      bool boundaryL = (i == 0) || !IsWord(s[i - 1]);
      // time?
      if (boundaryL && (len == 1 || len == 2) && j < n && s[j] == ':' &&
          (i == 0 || s[i - 1] != ':')) {
        int hour = std::atoi(s.substr(i, len).c_str());
        if (hour >= 1 && hour <= 12 && j + 2 < n + 1 && j + 1 < n &&
            IsDigit(s[j + 1]) && j + 2 < n && IsDigit(s[j + 2])) {
          int mn = std::atoi(s.substr(j + 1, 2).c_str());
          size_t after = j + 3;
          bool boundaryR = (after >= n) || !IsWord(s[after]);
          bool notColon = (after >= n) || s[after] != ':';
          if (mn >= 0 && mn <= 59 && boundaryR && notColon) {
            out += ExpandNumberTime(s.substr(i, after - i));
            i = after;
            continue;
          }
        }
      }
      // 4-digit year with optional trailing 's', whole word
      if (boundaryL && len == 4) {
        size_t end = j;
        bool hasS = (end < n && s[end] == 's');
        size_t wend = hasS ? end + 1 : end;
        bool boundaryR = (wend >= n) || !IsWord(s[wend]);
        if (boundaryR) {
          out += ExpandNumberTime(s.substr(i, wend - i));
          i = wend;
          continue;
        }
      }
      // not a year/time: copy the digit run verbatim
      out.append(s, i, len);
      i = j;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

// Strip thousands separators: (?<=\d),(?=\d) -> ""
string PassStripThousands(const string& s) {
  string out;
  for (size_t i = 0; i < s.size(); i++) {
    if (s[i] == ',' && i > 0 && IsDigit(s[i - 1]) && i + 1 < s.size() && IsDigit(s[i + 1]))
      continue;
    out += s[i];
  }
  return out;
}

// Currency: [$£]\d+(?:\.\d+)?(?: hundred| thousand| (?:[bm]|tr)illion)*\b | [$£]\d+\.\d\d?\b
// £ is UTF-8 0xC2 0xA3.
bool IsCurrencyStart(const string& s, size_t i) {
  if (s[i] == '$') return true;
  return (unsigned char)s[i] == 0xC2 && i + 1 < s.size() && (unsigned char)s[i + 1] == 0xA3;
}
string PassCurrency(const string& s) {
  static const char* scales[] = {" hundred", " thousand", " billion", " million", " trillion"};
  string out;
  size_t i = 0, n = s.size();
  while (i < n) {
    if (IsCurrencyStart(s, i)) {
      size_t symLen = (s[i] == '$') ? 1 : 2;
      size_t k = i + symLen;
      size_t ds = k;
      while (k < n && IsDigit(s[k])) k++;
      if (k > ds) {                 // has at least one digit
        // optional .\d+
        if (k < n && s[k] == '.') {
          size_t d2 = k + 1;
          while (d2 < n && IsDigit(s[d2])) d2++;
          if (d2 > k + 1) k = d2;
        }
        // optional scale words (repeated)
        for (;;) {
          bool matched = false;
          for (const char* sc : scales) {
            size_t l = std::string(sc).size();
            if (s.compare(k, l, sc) == 0) { k += l; matched = true; break; }
          }
          if (!matched) break;
        }
        // build the $ + rest (symbol as '$'/'£' -> pass first char marker)
        string matchStr;
        matchStr += (s[i] == '$') ? '$' : '#';  // '#' placeholder = pound
        matchStr += s.substr(i + symLen, k - (i + symLen));
        // ExpandCurrency keys on e[0]=='$'; for pound pass a sentinel it treats as pound
        out += ExpandCurrency(s[i] == '$' ? ("$" + s.substr(i + symLen, k - (i + symLen)))
                                          : ("#" + s.substr(i + symLen, k - (i + symLen))));
        i = k;
        continue;
      }
    }
    out += s[i];
    i++;
  }
  return out;
}

// Remaining decimals: \d*\.\d+ -> g
string PassDecimals(const string& s) {
  string out;
  size_t i = 0, n = s.size();
  while (i < n) {
    if (IsDigit(s[i]) || (s[i] == '.' && i + 1 < n && IsDigit(s[i + 1]))) {
      size_t j = i;
      while (j < n && IsDigit(s[j])) j++;
      if (j < n && s[j] == '.' && j + 1 < n && IsDigit(s[j + 1])) {
        size_t d2 = j + 1;
        while (d2 < n && IsDigit(s[d2])) d2++;
        out += ExpandDecimal(s.substr(i, d2 - i));
        i = d2;
        continue;
      }
      out.append(s, i, j - i);
      i = j;
      continue;
    }
    out += s[i];
    i++;
  }
  return out;
}

// (?<=\d)-(?=\d) -> " to "
string PassRanges(const string& s) {
  string out;
  for (size_t i = 0; i < s.size(); i++) {
    if (s[i] == '-' && i > 0 && IsDigit(s[i - 1]) && i + 1 < s.size() && IsDigit(s[i + 1]))
      out += " to ";
    else
      out += s[i];
  }
  return out;
}

// Possessive / plural spelling:
//   (?<=\d)S -> " S"
//   (?<=[BCDFGHJ-NP-TV-Z])'?s\b -> "'S"     (consonant capital, incl. optional ')
//   (?<=X')S\b -> "s"
bool IsConsonantCap(char c) {
  return IsUpper(c) && c != 'A' && c != 'E' && c != 'I' && c != 'O' && c != 'U';
}
string PassPossessive(const string& s) {
  string out;
  size_t n = s.size();
  for (size_t i = 0; i < n;) {
    char c = s[i];
    // (?<=X')S\b  -> "s"
    if (c == 'S' && i >= 2 && s[i - 1] == '\'' && s[i - 2] == 'X') {
      bool bR = (i + 1 >= n) || !IsWord(s[i + 1]);
      if (bR) { out += 's'; i++; continue; }
    }
    // (?<=[consonantCap])'?s\b -> "'S"   (match "s" or "'s" after a consonant cap)
    if (c == '\'' && i + 1 < n && s[i + 1] == 's') {
      bool bR = (i + 2 >= n) || !IsWord(s[i + 2]);
      if (bR && !out.empty() && IsConsonantCap(out.back())) { out += "'S"; i += 2; continue; }
    }
    if (c == 's') {
      bool bR = (i + 1 >= n) || !IsWord(s[i + 1]);
      if (bR && !out.empty() && IsConsonantCap(out.back())) { out += "'S"; i++; continue; }
    }
    // (?<=\d)S -> " S"
    if (c == 'S' && !out.empty() && IsDigit(out.back())) { out += " S"; i++; continue; }
    out += c;
    i++;
  }
  return out;
}

// Acronyms:
//   (?:[A-Za-z]\.){2,} [a-z]  -> replace the dots in the A.B.C. run with '-'
//   (?<=[A-Z])\.(?=[A-Z]) i   -> "-"
string PassAcronyms(const string& s) {
  // second rule first is order-sensitive in JS (done after the first). Replicate order.
  // rule 1: (?:[A-Za-z]\.){2,} followed by " " + [a-z]
  string a;
  size_t n = s.size();
  for (size_t i = 0; i < n;) {
    // try to match a run of ([A-Za-z].) at least twice
    size_t j = i, count = 0;
    while (j + 1 < n && IsAlpha(s[j]) && s[j + 1] == '.') { j += 2; count++; }
    if (count >= 2 && j + 1 < n && s[j] == ' ' && s[j + 1] >= 'a' && s[j + 1] <= 'z') {
      for (size_t k = i; k < j; k++) a += (s[k] == '.') ? '-' : s[k];
      i = j;
      continue;
    }
    a += s[i];
    i++;
  }
  // rule 2: letter '.' letter (both letters, case-insensitive) -> letter '-' letter
  string b;
  n = a.size();
  for (size_t i = 0; i < n; i++) {
    if (a[i] == '.' && i > 0 && IsAlpha(a[i - 1]) && i + 1 < n && IsAlpha(a[i + 1]))
      b += '-';
    else
      b += a[i];
  }
  return b;
}

// Whitespace: [^\S \n] -> ' ' (tab/CR/VT/FF), then "  +" -> ' ',
// then (?<=\n) +(?=\n) -> "" (spaces on otherwise-blank lines).
string PassWhitespace(const string& s) {
  string a;
  for (char c : s) {
    if (c == '\t' || c == '\r' || c == '\v' || c == '\f') a += ' ';
    else a += c;
  }
  // collapse 2+ spaces
  string b;
  for (size_t i = 0; i < a.size(); i++) {
    if (a[i] == ' ' && !b.empty() && b.back() == ' ') continue;
    b += a[i];
  }
  // spaces between newlines
  string c;
  for (size_t i = 0; i < b.size(); i++) {
    if (b[i] == ' ') {
      // preceded by \n (skipping already-emitted spaces) and followed by \n?
      bool prevNl = !c.empty() && c.back() == '\n';
      size_t k = i;
      while (k < b.size() && b[k] == ' ') k++;
      bool nextNl = k < b.size() && b[k] == '\n';
      if (prevNl && nextNl) { i = k - 1; continue; }
    }
    c += b[i];
  }
  return c;
}

// Word-boundary replace for the title abbreviations. `caps`=true also matches the
// all-caps form but only before " [A-Z]".
string PassTitles(const string& s) {
  string out;
  size_t n = s.size();
  auto wbL = [&](size_t i) { return i == 0 || !IsWord(s[i - 1]); };
  for (size_t i = 0; i < n;) {
    // Dr. / DR.  -> Doctor   (before " [A-Z]")
    if (wbL(i) && (s[i] == 'D') && i + 2 < n && (s[i + 1] == 'r' || s[i + 1] == 'R') &&
        s[i + 2] == '.') {
      if (i + 4 < n && s[i + 3] == ' ' && IsUpper(s[i + 4])) {
        out += "Doctor"; i += 3; continue;
      }
    }
    // Mr. -> Mister ; MR. -> Mister (before " [A-Z]")
    auto title = [&](const char* mixed, const char* caps, const char* to) -> bool {
      size_t l = std::string(mixed).size();
      if (wbL(i) && s.compare(i, l, mixed) == 0) { out += to; i += l; return true; }
      if (wbL(i) && s.compare(i, l, caps) == 0 && i + l + 1 < n && s[i + l] == ' ' &&
          IsUpper(s[i + l + 1])) { out += to; i += l; return true; }
      return false;
    };
    if (title("Mr.", "MR.", "Mister")) continue;
    if (title("Ms.", "MS.", "Miss")) continue;
    if (title("Mrs.", "MRS.", "Mrs")) continue;
    // etc. -> etc  (case-insensitive, NOT before " [A-Z]")
    if (wbL(i) && (Lower(s[i]) == 'e') && i + 3 < n && Lower(s[i + 1]) == 't' &&
        Lower(s[i + 2]) == 'c' && s[i + 3] == '.') {
      bool notCapNext = !(i + 5 < n && s[i + 4] == ' ' && IsUpper(s[i + 5]));
      if (notCapNext) { out += s.substr(i, 3); i += 4; continue; }
    }
    out += s[i];
    i++;
  }
  return out;
}

// \b(y)eah?\b -> "$1e'a"  (case-insensitive; keep the y's case)
string PassYeah(const string& s) {
  string out;
  size_t n = s.size();
  for (size_t i = 0; i < n;) {
    bool wbL = (i == 0) || !IsWord(s[i - 1]);
    if (wbL && Lower(s[i]) == 'y' && i + 2 < n && Lower(s[i + 1]) == 'e' &&
        Lower(s[i + 2]) == 'a') {
      size_t end = i + 3;
      if (end < n && Lower(s[end]) == 'h') end++;  // optional h
      bool wbR = (end >= n) || !IsWord(s[end]);
      if (wbR) { out += s[i]; out += "e'a"; i = end; continue; }
    }
    out += s[i];
    i++;
  }
  return out;
}

// Quote/punct folding (Stage 1 head). Order matters (see spec).
string PassQuotes(const string& s) {
  string t = s;
  ReplaceAll(t, "\xE2\x80\x98", "'");         // ‘
  ReplaceAll(t, "\xE2\x80\x99", "'");         // ’
  ReplaceAll(t, "\xC2\xAB", "\xE2\x80\x9C");  // « -> “
  ReplaceAll(t, "\xC2\xBB", "\xE2\x80\x9D");  // » -> ”
  ReplaceAll(t, "\xE2\x80\x9C", "\"");        // “ -> "
  ReplaceAll(t, "\xE2\x80\x9D", "\"");        // ” -> "
  ReplaceAll(t, "(", "\xC2\xAB");             // ( -> «
  ReplaceAll(t, ")", "\xC2\xBB");             // ) -> »
  ReplaceAll(t, "\xE3\x80\x81", ", ");        // 、
  ReplaceAll(t, "\xE3\x80\x82", ". ");        // 。
  ReplaceAll(t, "\xEF\xBC\x81", "! ");        // ！
  ReplaceAll(t, "\xEF\xBC\x8C", ", ");        // ，
  ReplaceAll(t, "\xEF\xBC\x9A", ": ");        // ：
  ReplaceAll(t, "\xEF\xBC\x9B", "; ");        // ；
  ReplaceAll(t, "\xEF\xBC\x9F", "? ");        // ？
  return t;
}

string Trim(const string& s) {
  size_t a = 0, b = s.size();
  while (a < b && (unsigned char)s[a] <= ' ') a++;
  while (b > a && (unsigned char)s[b - 1] <= ' ') b--;
  return s.substr(a, b - a);
}

}  // namespace

std::string Normalize(const std::string& utf8) {
  string s = PassQuotes(utf8);
  s = PassWhitespace(s);
  s = PassTitles(s);
  s = PassYeah(s);
  s = PassNumbers(s);
  s = PassStripThousands(s);
  s = PassCurrency(s);
  s = PassDecimals(s);
  s = PassRanges(s);
  s = PassPossessive(s);
  s = PassAcronyms(s);
  return Trim(s);
}

std::vector<Segment> SplitSegments(const std::string& s) {
  // Punctuation set d = ;:,.!?¡¿—…"«»“”(){}[]  (some multi-byte). Match runs of
  // (\s* punct+ \s*)+. Build a predicate over codepoints.
  static const char* kPunct[] = {
      ";", ":", ",", ".", "!", "?", "\"",
      "\xC2\xA1", "\xC2\xBF",              // ¡ ¿
      "\xE2\x80\x94", "\xE2\x80\xA6",       // — …
      "\xC2\xAB", "\xC2\xBB",               // « »
      "\xE2\x80\x9C", "\xE2\x80\x9D",       // “ ”
      "(", ")", "{", "}", "[", "]"};
  auto punctLenAt = [&](size_t i) -> int {  // >0 = byte length of a punct codepoint here
    for (const char* p : kPunct) {
      size_t l = std::string(p).size();
      if (s.compare(i, l, p) == 0) return (int)l;
    }
    return 0;
  };
  std::vector<Segment> segs;
  size_t i = 0, n = s.size();
  size_t textStart = 0;
  while (i < n) {
    // try to start a punctuation run at i: optional spaces, then >=1 punct, then the
    // greedy (\s* punct+ \s*)+ form.
    size_t j = i;
    while (j < n && s[j] == ' ') j++;
    if (j < n && punctLenAt(j) > 0) {
      // consume the full run
      size_t runEnd = i;
      size_t k = i;
      bool any = false;
      for (;;) {
        size_t m = k;
        while (m < n && s[m] == ' ') m++;
        int pl = (m < n) ? punctLenAt(m) : 0;
        if (pl == 0) break;
        while (m < n) {
          int q = punctLenAt(m);
          if (q == 0) break;
          m += q;
        }
        while (m < n && s[m] == ' ') m++;
        k = m; any = true; runEnd = m;
      }
      if (any) {
        if (i > textStart) segs.push_back({false, s.substr(textStart, i - textStart)});
        segs.push_back({true, s.substr(i, runEnd - i)});
        i = runEnd;
        textStart = i;
        continue;
      }
    }
    i += Utf8Len((unsigned char)s[i]);
  }
  if (textStart < n) segs.push_back({false, s.substr(textStart)});
  return segs;
}

std::string PostProcess(const std::string& phon) {
  string s = phon;
  ReplaceAll(s, "k\xC9\x99k\xCB\x88o\xCB\x90\xC9\xB9o\xCA\x8A",
             "k\xCB\x88o\xCA\x8Ak\xC9\x99\xC9\xB9o\xCA\x8A");  // kəkˈoːɹoʊ -> kˈoʊkəɹoʊ
  ReplaceAll(s, "k\xC9\x99k\xCB\x88\xC9\x94\xCB\x90\xC9\xB9\xC9\x99\xCA\x8A",
             "k\xCB\x88\xC9\x99\xCA\x8Ak\xC9\x99\xC9\xB9\xC9\x99\xCA\x8A");  // kəkˈɔːɹəʊ -> kˈəʊkəɹəʊ
  ReplaceAll(s, "\xCA\xB2", "j");   // ʲ -> j
  ReplaceAll(s, "r", "\xC9\xB9");   // r -> ɹ
  ReplaceAll(s, "x", "k");          // x -> k
  ReplaceAll(s, "\xC9\xAC", "l");   // ɬ -> l
  // (?<=[a-zɹː])(?=hˈʌndɹɪd) -> insert space before "hundred"
  {
    static const string hundred = "h\xCB\x88\xCA\x8Cnd\xC9\xB9\xC9\xAA" "d";  // hˈʌndɹɪd
    string out; size_t i = 0, n = s.size();
    while (i < n) {
      if (s.compare(i, hundred.size(), hundred) == 0 && i > 0) {
        // preceding codepoint in [a-z] or ɹ(C9 B9) or ː(CB 90)?
        char pc = s[i - 1];
        bool prevLow = (pc >= 'a' && pc <= 'z');
        bool prevRp = i >= 2 && (unsigned char)s[i - 2] == 0xC9 && (unsigned char)s[i - 1] == 0xB9;
        bool prevLen = i >= 2 && (unsigned char)s[i - 2] == 0xCB && (unsigned char)s[i - 1] == 0x90;
        if (prevLow || prevRp || prevLen) out += ' ';
      }
      out += s[i]; i++;
    }
    s = out;
  }
  // " z" before terminal punctuation/space/end -> "z"
  {
    string out; size_t i = 0, n = s.size();
    while (i < n) {
      if (s[i] == ' ' && i + 1 < n && s[i + 1] == 'z') {
        size_t after = i + 2;
        bool term = (after >= n) || s[after] == ' ';
        static const char* enders[] = {";", ":", ",", ".", "!", "?", "\"",
                                        "\xC2\xA1", "\xC2\xBF", "\xE2\x80\x94",
                                        "\xE2\x80\xA6", "\xC2\xAB", "\xC2\xBB",
                                        "\xE2\x80\x9C", "\xE2\x80\x9D"};
        for (const char* e : enders)
          if (s.compare(after, std::string(e).size(), e) == 0) { term = true; break; }
        if (term) { out += 'z'; i += 2; continue; }
      }
      out += s[i]; i++;
    }
    s = out;
  }
  // en-us: (?<=nˈaɪn)ti(?!ː) -> "di"
  {
    static const string nine = "n\xCB\x88\x61\xC9\xAAn";  // nˈaɪn
    string out; size_t i = 0, n = s.size();
    while (i < n) {
      if (s.compare(i, 2, "ti") == 0 && out.size() >= nine.size() &&
          out.compare(out.size() - nine.size(), nine.size(), nine) == 0) {
        bool nextLen = (i + 2 + 1 < n) && (unsigned char)s[i + 2] == 0xCB &&
                       (unsigned char)s[i + 3] == 0x90;  // ː
        if (!nextLen) { out += "di"; i += 2; continue; }
      }
      out += s[i]; i++;
    }
    s = out;
  }
  return Trim(s);
}

}  // namespace kokoro_text
