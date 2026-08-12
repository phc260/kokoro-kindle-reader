// The two ONNX sessions and the dictionary that decodes what the second one emits.
//
// Built once, on the worker thread, and kept for as long as the process lives. Building them
// reads ~10 MiB of model and costs roughly a third of a second, which is the whole reason
// `probe()` answers `/status` from the file system instead.
//
// CPU, by measurement — see `PROVIDER` in lib.rs. There is no live provider switch here and
// there should not be one:
// unlike the synth, where the user picks an engine in the panel and a page of narration is
// seconds of audio, OCR is a few hundred milliseconds behind a page turn and the answer is
// already known.

use ort::ep::CPU;
use ort::session::{builder::GraphOptimizationLevel, Session};

use crate::Assets;

/// Both sessions plus the decoded alphabet.
pub struct Models {
    pub det: Session,
    pub rec: Session,
    /// Class `c` of the recognizer's output is `charset[c - 1]`; class 0 is the CTC blank.
    pub charset: Vec<String>,
}

impl Models {
    /// Load everything, or say precisely what is wrong with it.
    ///
    /// Every failure here is `Unavailable` at the call site: a missing model, a session that
    /// will not build and a dictionary that does not match the recognizer are all things no
    /// amount of retrying this page will fix.
    ///
    /// **The digests gate the load, and they gate the exact bytes that get loaded.** Verifying
    /// them in `probe()` alone made the pin decorative where it mattered: `/status` would answer
    /// `corrupt` while `/ocr` went on recognizing with whatever was on disk, so the two
    /// endpoints disagreed about whether the engine was usable and the permissive one was the
    /// dangerous one. A recognizer that is not the pinned one but happens to emit the same class
    /// count passes every other check in this crate and returns fluent, plausible, entirely
    /// wrong text.
    ///
    /// Calling `probe()` from here would have been the obvious fix and it is not enough, for two
    /// reasons that are the same reason: **a path is not a file.** `probe` hashes what is at the
    /// path *now* and caches that against the file's length and mtime, so a replacement of the
    /// same length with a restored mtime reads as verified; and even with the cache defeated,
    /// nothing stops the file changing between the check and the `commit_from_file` that reopens
    /// it. So each file is read ONCE, hashed as bytes, and the session and dictionary are built
    /// from that same buffer. What was verified is what runs, with nothing in between.
    ///
    /// Costs ~10 MiB of transient allocation and one hash of it, once per process — the models
    /// are kept after a successful load. The synth does the same thing with a 325 MB model.
    pub fn open(assets: &Assets) -> Result<Models, String> {
        let charset = load_charset(&read_pinned(&assets.dictionary(), crate::DICT_SHA256)?)?;
        let det = build(&assets.detector(), &read_pinned(&assets.detector(), crate::DET_SHA256)?)?;
        let rec =
            build(&assets.recognizer(), &read_pinned(&assets.recognizer(), crate::REC_SHA256)?)?;
        Ok(Models { det, rec, charset })
    }
}

/// Read a pinned file and prove it is the one this code was written against.
///
/// The bytes are returned rather than the verdict, because handing back "yes, that path is fine"
/// is exactly the check-then-reopen gap this exists to close.
fn read_pinned(path: &std::path::Path, expected: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("{} is not there", path.display())
        } else {
            format!("{}: {e}", path.display())
        }
    })?;
    let actual = crate::digest_of(&bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!("{} hashes to {actual}, not {expected}", path.display()));
    }
    Ok(bytes)
}

/// Build a session from bytes already proven to be the pinned ones.
///
/// `commit_from_memory`, never `commit_from_file`: reopening the path would hand ORT whatever is
/// there at that instant, which need not be what was hashed a moment earlier. `path` survives
/// only so a failure can name which of the two models would not load.
fn build(path: &std::path::Path, bytes: &[u8]) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| e.to_string())?
        .with_execution_providers([CPU::default().build()])
        .map_err(|e| e.to_string())?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?
        .commit_from_memory(bytes)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Read the recognizer's dictionary into the class order the CTC head emits.
///
/// Three details, and each of them silently corrupts every word on the page if missed:
///
///   * **The file opens with an empty sentinel line**, which is the upstream dictionary's own
///     placeholder for the blank. This decoder puts the blank at class 0 itself, so keeping
///     the sentinel would insert it twice and shift the entire alphabet by one — every
///     character decoded as its neighbour.
///   * **A trailing space class is appended**, because `use_space_char` is what the model was
///     exported with. That class is the only thing that separates words: without it the
///     recognizer emits one run-together string with nothing to split on, and word boxes,
///     `hasOutlierGap` and the highlight all lose their input at once.
///   * **Only the line terminator is stripped.** A dictionary entry can legitimately be a
///     space or a punctuation mark, so trimming whitespace would delete a class and shift
///     everything after it.
fn load_charset(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| format!("the dictionary is not UTF-8: {e}"))?
        .to_string();

    let mut charset: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();
    // A file ending in a newline yields a final empty piece that is not a class.
    if charset.last().is_some_and(String::is_empty) {
        charset.pop();
    }
    if charset.first().is_some_and(String::is_empty) {
        charset.remove(0);
    }
    if charset.is_empty() {
        return Err("the dictionary holds no characters".to_string());
    }
    charset.push(" ".to_string());
    Ok(charset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charset_of(body: &str) -> Result<Vec<String>, String> {
        load_charset(body.as_bytes())
    }

    #[test]
    fn the_leading_sentinel_is_dropped_and_a_space_appended() {
        assert_eq!(charset_of("\na\nb\n").unwrap(), vec!["a", "b", " "]);
    }

    #[test]
    fn a_dictionary_without_the_sentinel_keeps_every_class() {
        assert_eq!(charset_of("a\nb").unwrap(), vec!["a", "b", " "]);
    }

    #[test]
    fn a_space_entry_survives_being_read() {
        // Trimming rather than stripping the terminator would delete this class and shift
        // every character after it by one.
        assert_eq!(charset_of("\n \n!\n").unwrap(), vec![" ", "!", " "]);
    }

    #[test]
    fn crlf_does_not_become_part_of_a_class() {
        assert_eq!(charset_of("\r\na\r\nb\r\n").unwrap(), vec!["a", "b", " "]);
    }

    #[test]
    fn an_empty_dictionary_is_an_error_not_a_one_class_alphabet() {
        assert!(charset_of("\n").is_err());
    }

    #[test]
    fn a_dictionary_that_is_not_utf8_is_an_error_not_a_panic() {
        assert!(load_charset(&[0xff, 0xfe, 0x00]).is_err());
    }

    #[test]
    fn a_missing_file_names_itself() {
        let err = read_pinned(std::path::Path::new("no-such-model.onnx"), crate::DET_SHA256)
            .unwrap_err();
        assert!(err.contains("no-such-model.onnx"), "{err}");
        assert!(err.contains("not there"), "{err}");
    }

    #[test]
    fn bytes_that_miss_the_pin_are_refused_and_never_returned() {
        // The gate returns the BYTES, not a verdict about a path: handing back "that path is
        // fine" is the check-then-reopen gap this exists to close.
        let path = std::env::temp_dir().join("kokoro-ocr-unpinned.bin");
        std::fs::write(&path, b"not the pinned model").unwrap();
        let err = read_pinned(&path, crate::DET_SHA256).unwrap_err();
        assert!(err.contains("hashes to"), "{err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bytes_that_match_the_pin_come_back_intact() {
        let body = b"pin me";
        let path = std::env::temp_dir().join("kokoro-ocr-pinned.bin");
        std::fs::write(&path, body).unwrap();
        let digest = crate::digest_of(body);
        assert_eq!(read_pinned(&path, &digest).unwrap(), body);
        // Case-insensitively, since the pins are written lowercase and Get-FileHash is upper.
        assert!(read_pinned(&path, &digest.to_uppercase()).is_ok());
        let _ = std::fs::remove_file(path);
    }
}
