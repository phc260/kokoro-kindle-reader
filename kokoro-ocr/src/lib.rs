// PP-OCR detection + recognition for the Kindle Cloud Reader path.
//
// WHAT THIS CRATE IS FOR. Recognition on the host, reached over one authenticated loopback
// endpoint, so the extension ships no engine, no wasm, no language data and no CSP relaxation
// to run any of it.
//
// WHY TWO MODELS AND NOT ONE ENGINE. Finding text among artwork is a different job from
// reading it, and a real Cloud Reader picture-book page — four sparse lines of large serif type
// in the corner of a full-page illustration — is the first job, not the second. PP-OCR
// separates them by construction: a DBNet detector emits a text-probability map, and only the
// regions it finds are handed to a CTC recognizer. That separation is also what makes the
// running head and the folio arrive as TWO lines instead of one, which the extension's
// furniture policy requires to work at all. The rules below were arrived at by measuring this
// pair against the alternatives, and several of them INVERT what a general-purpose OCR engine
// would want — so read what each one says rather than reasoning from such an engine's
// properties. `../README.md` is where the argument lives.
//
// WHAT IT DELIBERATELY DOES NOT DO. Nothing in here decides what gets narrated. The dark-page
// check, the gutter split, the missed-gutter retry, evidence-based furniture suppression and
// its cross-page memory, text cleanup and character offsets all stay in the extension's
// `src/ocr/`, unchanged. Those rules can silently remove a line of the book, they were paid
// for four content losses at a time, and swapping the engine underneath them is already the
// whole change — moving them at the same time would make any regression impossible to
// attribute. This crate returns the RAW structure those rules consume: lines in reading order,
// each a list of words with text, confidence and a rectangle.
//
// THE PUBLIC API IS TARGET-NEUTRAL. No HTTP, browser, Windows-UI, named-pipe or synthesis
// types appear in it, so a non-Windows port reuses this crate as-is and everything that
// knows about the transport lives in `kokoro-host/src/webserve.rs`.
//
// IT DOES NOT INITIALIZE ONNX RUNTIME EITHER. `ort`'s own guidance is that a library crate
// must not create the environment — the application does, once, and this one already does it
// in `native_synth`'s worker so both sessions load the same staged `onnxruntime.dll`. Nothing
// here depends on that having happened first: `ort`'s lazy path resolves the same DLL next to
// the exe, so whichever of the two touches a session first gets the same library.

mod detect;
mod engine;
mod prep;
mod recognize;
mod session;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

pub use engine::{JobHandle, Ocr};

/// The version of the `/ocr` response shape. A change here is a change to a contract two
/// codebases share.
pub const RESPONSE_VERSION: u32 = 1;

/// What the endpoint reports as its engine, and the two models behind it. Constants rather
/// than literals in the JSON so `/status` and the response cannot disagree about them, and
/// named so a capture can be traced back to exactly what read it.
pub const ENGINE_NAME: &str = "pp-ocr";
pub const DETECTOR_NAME: &str = "en-PP-OCRv3-det";
pub const RECOGNIZER_NAME: &str = "en-PP-OCRv5-mobile-rec";

/// English only for this release. Adding a language is a different recognizer, a different
/// dictionary and a different class count — three pinned files and a re-measured fixture set,
/// not a language pack dropped into a directory.
pub const LANGUAGE: &str = "eng";

/// The execution provider, measured rather than assumed: WebGPU was slower than CPU here for
/// warm recognition, and returned identical text and boxes. That is the expected shape —
/// detection is one small convolutional pass and recognition is one inference per line, so
/// neither amortizes a GPU upload. Recognition is also still unbatched, which is the assumption
/// most likely to change: if it does, measure again rather than inferring.
pub const PROVIDER: &str = "cpu";

// ------------------------------------------------------------------------------ geometry

/// A rectangle in the SUBMITTED image's pixels, top-left origin, `x1`/`y1` exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Rect {
    pub fn width(self) -> i32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> i32 {
        self.y1 - self.y0
    }

    fn is_empty(self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// Confine to `0..w` / `0..h`. Every rectangle that leaves this crate has been through
    /// here: a box is an index into someone's pixels — the crop below, the highlight in the
    /// browser — and one that runs off the page is not a wrong answer, it is a panic.
    fn clamped(self, w: i32, h: i32) -> Rect {
        Rect {
            x0: self.x0.clamp(0, w),
            y0: self.y0.clamp(0, h),
            x1: self.x1.clamp(0, w),
            y1: self.y1.clamp(0, h),
        }
    }
}

/// One recognized word.
#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    /// 0-100, to match the shape the extension already consumes. Here it is the mean of the
    /// CTC probabilities of the characters that make the word, times 100. Nothing in the
    /// extension thresholds on it — it is summed into a mean and logged — but it crosses
    /// because it is part of the incumbent shape.
    pub confidence: f32,
    pub bbox: Rect,
}

/// One line of words.
///
/// A line here is a DETECTED region, not a grouping decided after the fact. That is the shape
/// the furniture rules were written for: a running head is a line that repeats, and word
/// spacing within a line is what tells a justified body line from a title-left/folio-right
/// header. The engine that finds the region is the one that knows where the line ends.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub words: Vec<Word>,
}

/// One recognized column.
#[derive(Clone, Debug)]
pub struct Page {
    /// The submitted image's size, echoed back so a client can prove the coordinate space.
    pub width: u32,
    pub height: u32,
    pub lines: Vec<Line>,
    /// The two stages, timed separately. They fail and scale differently — detection is one
    /// pass over the page, recognition is one inference per line found — and a page that is
    /// slow because it has forty lines on it is a different fact from one that is slow because
    /// the detector is grinding.
    pub detect_ms: f64,
    pub recognize_ms: f64,
    pub ocr_ms: f64,
}

// -------------------------------------------------------------------------------- bounds

/// Everything that bounds one request.
///
/// These are the caller's to set because the caller is the one facing the network. The
/// defaults are sized for a Cloud Reader column at a plausible zoom with room to spare, not
/// for the largest image a browser could produce.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Cap on the compressed body. Enforced by the transport BEFORE this crate sees it as
    /// well — checking a length after buffering is not a limit — and re-checked here so a
    /// caller that forgets cannot hand the decoder something unbounded.
    pub max_body_bytes: usize,
    /// Cap per side, so a 1 x 400000 strip is refused on its shape rather than its area.
    pub max_dimension: u32,
    /// Cap on decoded pixels, checked against the PNG header before anything is allocated.
    pub max_pixels: u64,
    /// How long one page gets **once the worker starts on it** — decode, detection and
    /// recognition. It is not the caller's overall timeout, and deliberately excludes two
    /// things:
    ///
    ///   * **queue time**, because a page fourth in line behind three others is not a page
    ///     these models are struggling with, and only one of those is worth asking about
    ///     again later. That is the transport's to bound (`OCR_REQUEST_TIMEOUT` in
    ///     `webserve.rs`), and it needs both numbers;
    ///   * **the first load**, because building the two sessions reads ~10 MiB and costs about
    ///     a third of a second. Charging that to whichever page happened to arrive first would
    ///     fail a page for the sin of being first.
    ///
    /// ORT has no cancellation callback, so it is checked BETWEEN stages and between lines
    /// rather than inside a run — which is finer-grained than it sounds, because recognition
    /// is one inference per line and each is tens of milliseconds. Detection is the one
    /// indivisible step, and it is the fast one.
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // A 1200x1700 PNG column of book text is well under 1 MiB, and sizing this from
            // that refused a real Cloud Reader page: a full-page colour plate captured at the
            // reader's own device-pixel resolution and re-encoded LOSSLESSLY to PNG is an
            // order of magnitude larger than the JPEG it was drawn from. The transport must
            // carry the same number (`MAX_OCR_BODY` in `webserve.rs`).
            max_body_bytes: 32 * 1024 * 1024,
            max_dimension: 10_000,
            // 40 Mpx — roughly a 6000x6600 page, far past anything a reader renders. The
            // detector downsamples to `detect::MAX_SIDE` regardless, so this bounds the
            // decode and the line crops, not the model input.
            max_pixels: 40_000_000,
            deadline: Duration::from_secs(30),
        }
    }
}

// -------------------------------------------------------------------------------- errors

/// Every way one request can fail, kept distinguishable on purpose.
///
/// "OCR failed" is the answer that made the browser engine hard to live with: a model that
/// would not load, a timeout and a page the engine could not read all arrived as the same
/// string, so the user was told to retry the one thing retrying could not fix. Each of these
/// maps to a different thing for the reader to do.
#[derive(Clone, Debug)]
pub enum Error {
    /// The engine could not be brought up at all — models missing, corrupt, a dictionary that
    /// does not match the recognizer, or a session that would not build. Retrying this page
    /// will not help.
    Unavailable(String),
    /// The bytes are not a decodable image.
    Decode(String),
    /// The request is outside `Limits`.
    TooLarge(String),
    /// The deadline expired.
    Timeout,
    /// The caller asked for this to stop — a Stop, or a browser that went away.
    Cancelled,
    /// A model ran and failed.
    Recognize(String),
    /// The bounded queue is full. The one error that means "ask again in a moment".
    Busy,
}

impl Error {
    /// A stable machine-readable tag for the wire. The prose in the variants is for a human
    /// reading a log; this is what a client branches on.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Unavailable(_) => "unavailable",
            Error::Decode(_) => "decode",
            Error::TooLarge(_) => "too_large",
            Error::Timeout => "timeout",
            Error::Cancelled => "cancelled",
            Error::Recognize(_) => "recognize",
            Error::Busy => "busy",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unavailable(m) => write!(f, "OCR unavailable: {m}"),
            Error::Decode(m) => write!(f, "cannot decode the image: {m}"),
            Error::TooLarge(m) => write!(f, "image rejected: {m}"),
            Error::Timeout => write!(f, "recognition timed out"),
            Error::Cancelled => write!(f, "recognition cancelled"),
            Error::Recognize(m) => write!(f, "recognition failed: {m}"),
            Error::Busy => write!(f, "the OCR worker is busy"),
        }
    }
}

impl std::error::Error for Error {}

// -------------------------------------------------------------------- cancellation token

/// A one-way flag the caller can raise while a job runs.
///
/// ORT exposes no way to abandon a run in progress, so raising this is a bounded discard
/// contract rather than a promise that the current inference stops. What it does stop is
/// everything after it: the worker checks between stages and before each line, and a page is
/// tens of lines, so the wait is one line's inference and not one page's. A job that is
/// already inside a run finishes it and its result is thrown away — safe here precisely
/// because the cross-page furniture memory lives in the extension, so a late result has
/// nothing left to poison.
#[derive(Debug, Default)]
pub struct Cancel(AtomicBool);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

// ------------------------------------------------------------------------------- assets

/// The three files this engine is, and what they are supposed to be.
///
/// Names are fixed rather than configurable. They are not a deployment choice: the digests
/// below pin exactly which bytes this code was written against, and a directory holding
/// something else is a broken install, not a variant.
pub const DET_FILE: &str = "det.onnx";
pub const REC_FILE: &str = "rec.onnx";
pub const DICT_FILE: &str = "en_dict.txt";

/// The pinned SHA-256 of each. The upstream source and the pinned revision for every file are
/// in `native-deps/fetch-ocr-models.ps1`, which is what downloads them. Both sources are
/// Apache-2.0 ONNX conversions of PaddleOCR models: the detector from RapidOCR, the recognizer
/// and its dictionary from ppu-paddle-ocr-models.
///
/// Checked on every probe rather than only at install. The models are data reachable from a
/// network-facing endpoint, and a digest that is only verified by an installer is a digest
/// that stops being true the moment anything else writes to the directory.
pub const DET_SHA256: &str = "f139598bc2af4e4b6fe98dec11574e30edfdd91fc94ac1425c18ace3bd5a866b";
pub const REC_SHA256: &str = "1081b104a3c44d103511f150763d997a846994431c5775a800c802254c1124bf";
pub const DICT_SHA256: &str = "c60d46e9e01d500ed6388fe8681051eac9cf6692e0d57238315be171927a0a1b";

/// Where the models live.
#[derive(Clone, Debug)]
pub struct Assets {
    /// The directory holding all three files.
    pub dir: PathBuf,
}

impl Assets {
    pub fn new(dir: impl Into<PathBuf>) -> Assets {
        Assets { dir: dir.into() }
    }

    pub fn detector(&self) -> PathBuf {
        self.dir.join(DET_FILE)
    }

    pub fn recognizer(&self) -> PathBuf {
        self.dir.join(REC_FILE)
    }

    pub fn dictionary(&self) -> PathBuf {
        self.dir.join(DICT_FILE)
    }

    /// The three files with the digest each must hash to, in the order a probe reports them.
    fn pinned(&self) -> [(PathBuf, &'static str); 3] {
        [
            (self.detector(), DET_SHA256),
            (self.recognizer(), REC_SHA256),
            (self.dictionary(), DICT_SHA256),
        ]
    }
}

/// What `GET /status` reports about OCR, and what the extension gates on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    /// A file is not where it should be — an install that did not finish, or something
    /// deleted since.
    Missing,
    /// Present but not what was pinned. There is no "wrong version" state separate from this
    /// one: these models are pinned by digest, so a different build of the same model is
    /// exactly as much of a mismatch as a truncated download.
    Corrupt,
    /// Something else went wrong probing it.
    Error,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Ready => "ready",
            State::Missing => "missing",
            State::Corrupt => "corrupt",
            State::Error => "error",
        }
    }
}

/// The OCR component of the host's status.
#[derive(Clone, Debug)]
pub struct Status {
    pub state: State,
    pub engine: &'static str,
    pub detector: &'static str,
    pub recognizer: &'static str,
    pub language: &'static str,
    pub provider: &'static str,
    /// Why the state is not `ready`. Never populated on the happy path.
    pub detail: Option<String>,
}

/// Probe the assets WITHOUT building a session.
///
/// The distinction is the whole point of this function. Building the two sessions reads ~10
/// MiB of model and costs a third of a second, and `/status` is polled. So readiness is
/// answered from the file system, and nothing is loaded until a page actually needs
/// recognizing.
///
/// Each digest is cached against that file's own length and mtime: re-hashing 10 MiB on every
/// poll would be its own reason not to poll.
pub fn probe(assets: &Assets) -> Status {
    let mut status = Status {
        state: State::Ready,
        engine: ENGINE_NAME,
        detector: DETECTOR_NAME,
        recognizer: RECOGNIZER_NAME,
        language: LANGUAGE,
        provider: PROVIDER,
        detail: None,
    };

    for (path, expected) in assets.pinned() {
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                status.state = State::Missing;
                status.detail = Some(format!("{} is not there", path.display()));
                return status;
            }
            Err(e) => {
                status.state = State::Error;
                status.detail = Some(format!("{}: {e}", path.display()));
                return status;
            }
        };

        match cached_digest(&path, (meta.len(), meta.modified().ok())) {
            Ok(actual) if actual.eq_ignore_ascii_case(expected) => {}
            Ok(actual) => {
                status.state = State::Corrupt;
                status.detail =
                    Some(format!("{} hashes to {actual}, not {expected}", path.display()));
                return status;
            }
            Err(e) => {
                status.state = State::Error;
                status.detail = Some(format!("cannot hash {}: {e}", path.display()));
                return status;
            }
        }
    }

    status
}

/// The SHA-256 of some bytes, lowercase hex. The one hash implementation in this crate.
pub(crate) fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

type DigestKey = (u64, Option<SystemTime>);

/// One cache entry per pinned file, keyed by the path, so probing three files does not have
/// each one evict the last.
///
/// **This cache is for `/status` only.** Keying on length and mtime is right for a polled
/// readiness report and wrong for a gate: same-length bytes with a restored mtime read as
/// verified. `Models::open` therefore does not come through here — it hashes the buffer it is
/// about to hand ORT (`session::read_pinned`), which is also the only way to close the gap
/// between checking a path and reopening it.
fn cached_digest(path: &Path, key: DigestKey) -> std::io::Result<String> {
    static CACHE: Mutex<Vec<(PathBuf, DigestKey, String)>> = Mutex::new(Vec::new());

    if let Ok(guard) = CACHE.lock() {
        if let Some((_, _, digest)) =
            guard.iter().find(|(p, k, _)| p == path && *k == key)
        {
            return Ok(digest.clone());
        }
    }

    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    let digest = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();

    if let Ok(mut guard) = CACHE.lock() {
        guard.retain(|(p, _, _)| p != path);
        guard.push((path.to_path_buf(), key, digest.clone()));
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_are_missing_not_error() {
        let assets = Assets::new(std::env::temp_dir().join("kokoro-ocr-no-such-dir"));
        let status = probe(&assets);
        assert_eq!(status.state, State::Missing, "{status:?}");
        assert!(status.detail.unwrap().contains(DET_FILE));
    }

    #[test]
    fn a_stub_file_is_corrupt_not_ready() {
        let dir = std::env::temp_dir().join("kokoro-ocr-stub-models");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [DET_FILE, REC_FILE, DICT_FILE] {
            std::fs::write(dir.join(name), b"not a model").unwrap();
        }
        let status = probe(&Assets::new(&dir));
        assert_eq!(status.state, State::Corrupt, "{status:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_status_names_both_models() {
        // A capture has to be traceable to what read it; "pp-ocr" alone is two moving parts.
        let status = probe(&Assets::new("no-such-dir"));
        assert_eq!(status.engine, "pp-ocr");
        assert_eq!(status.detector, "en-PP-OCRv3-det");
        assert_eq!(status.recognizer, "en-PP-OCRv5-mobile-rec");
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(Error::Timeout.code(), "timeout");
        assert_eq!(Error::Cancelled.code(), "cancelled");
        assert_eq!(Error::Busy.code(), "busy");
        assert_eq!(Error::Decode(String::new()).code(), "decode");
    }

    #[test]
    fn a_cancel_flag_is_one_way() {
        let c = Cancel::default();
        assert!(!c.is_cancelled());
        c.cancel();
        c.cancel();
        assert!(c.is_cancelled());
    }

    #[test]
    fn clamping_confines_a_rect_to_the_image() {
        let r = Rect { x0: -4, y0: -1, x1: 200, y1: 40 };
        assert_eq!(r.clamped(10, 10), Rect { x0: 0, y0: 0, x1: 10, y1: 10 });
        let inside = Rect { x0: 1, y0: 1, x1: 9, y1: 9 };
        assert_eq!(inside.clamped(100, 100), inside);
    }
}
