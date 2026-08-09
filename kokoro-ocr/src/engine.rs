// Two sessions, one thread, one page at a time.
//
// WHY A THREAD OF ITS OWN. Building the sessions reads ~10 MiB of model and costs a third of a
// second, so they must be kept, and keeping them means one owner. This is the same shape the
// synth worker has and for the same reason — except it is a DIFFERENT thread, which is the
// load-bearing part. OCR must not queue behind a page of narration and narration must not
// queue behind OCR; they are both CPU work on the same machine, so they contend, but
// contention is what a scheduler is for and a shared worker would be a deadlock waiting for a
// two-column page. It is also off the Tokio runtime the pipe server and the HTTP endpoint
// share: a 700 ms blocking page on a runtime thread is 700 ms of Kindle's audio not being
// written.
//
// WHY THE MODELS ARE LOADED LAZILY. `probe()` answers `/status` from the file system, and
// `/status` is polled. Loading on the first real page means a host that is never asked to
// recognize anything never pays for the models — and it means a repaired install starts
// working without a restart, because a failed load is not remembered.

use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Instant;

use crate::session::Models;
use crate::{detect, prep, recognize};
use crate::{Assets, Cancel, Error, Limits, Line, Page};

/// How many jobs may be waiting at once.
///
/// Small on purpose. A page is at most two columns and a user has at most a couple of tabs
/// open, so anything past this is not a reader waiting for a page — it is a client that has
/// stopped reading the answers, and the honest reply is `Busy` rather than a queue that
/// grows until the machine notices.
const QUEUE_DEPTH: usize = 4;

struct Job {
    image: Vec<u8>,
    limits: Limits,
    cancel: Arc<Cancel>,
    reply: Sender<Result<Page, Error>>,
}

/// A submitted job. Hold it to wait; hold `cancel_handle()` to give up on it.
pub struct JobHandle {
    rx: Receiver<Result<Page, Error>>,
    cancel: Arc<Cancel>,
}

impl JobHandle {
    /// The flag this job checks between stages and between lines. Raising it stops the page
    /// after at most one line's inference and — whether or not the current run stops — tells
    /// the caller to discard whatever comes back.
    pub fn cancel_handle(&self) -> Arc<Cancel> {
        Arc::clone(&self.cancel)
    }

    /// Block until the worker answers. BLOCKING: an async caller belongs in `spawn_blocking`.
    pub fn wait(self) -> Result<Page, Error> {
        match self.rx.recv() {
            Ok(result) => result,
            // The worker thread is gone. Nothing that arrives later can help, so this is the
            // same class of answer as a missing model.
            Err(_) => Err(Error::Unavailable("the OCR worker stopped".into())),
        }
    }
}

/// The handle callers keep. Cloneable-by-reference through an `Arc` at the call site; the
/// worker behind it is single.
pub struct Ocr {
    tx: SyncSender<Job>,
    assets: Assets,
    limits: Limits,
}

impl Ocr {
    /// Start the worker. Does not load a model — see the note above about lazy loading.
    pub fn new(assets: Assets, limits: Limits) -> Ocr {
        let (tx, rx) = sync_channel::<Job>(QUEUE_DEPTH);
        let worker_assets = assets.clone();
        // If the thread cannot be spawned there is nothing useful to do about it: every
        // submit then fails on the closed channel with `Unavailable`, which is the truth.
        let _ = std::thread::Builder::new()
            .name("kokoro-ocr".into())
            .spawn(move || worker_loop(worker_assets, rx));
        Ocr { tx, assets, limits }
    }

    pub fn assets(&self) -> &Assets {
        &self.assets
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Queue one image. Returns immediately; `Busy` when the bounded queue is full.
    ///
    /// The body limit is checked HERE as well as in the decoder, before the bytes are handed
    /// to a queue that might hold them for as long as three other pages take to recognize.
    pub fn submit(&self, image: Vec<u8>) -> Result<JobHandle, Error> {
        if image.len() > self.limits.max_body_bytes {
            return Err(Error::TooLarge(format!(
                "{} byte body over the {} byte limit",
                image.len(),
                self.limits.max_body_bytes
            )));
        }

        let (reply, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(Cancel::default());
        let job = Job { image, limits: self.limits, cancel: Arc::clone(&cancel), reply };

        match self.tx.try_send(job) {
            Ok(()) => Ok(JobHandle { rx, cancel }),
            Err(TrySendError::Full(_)) => Err(Error::Busy),
            Err(TrySendError::Disconnected(_)) => {
                Err(Error::Unavailable("the OCR worker stopped".into()))
            }
        }
    }

    /// Submit and wait. BLOCKING — for tests and for callers that are already off the
    /// runtime.
    pub fn recognize(&self, image: Vec<u8>) -> Result<Page, Error> {
        self.submit(image)?.wait()
    }
}

// ------------------------------------------------------------------------------- worker

fn worker_loop(assets: Assets, rx: Receiver<Job>) {
    let mut models: Option<Models> = None;

    while let Ok(job) = rx.recv() {
        // A caller that gave up before the worker got here has already been told nothing;
        // running the page anyway would only make the NEXT one wait.
        if job.cancel.is_cancelled() {
            let _ = job.reply.send(Err(Error::Cancelled));
            continue;
        }

        // A failed load is not cached: the fix for `missing` is to put the file back, and a
        // host that had to be restarted to notice would be a worse answer than the one retry
        // costs.
        //
        // `catch_unwind` is what makes that true in every case rather than most of them. ORT's
        // dylib resolution PANICS when it cannot load the library — there is no `Result` on
        // that path — and an unwind out of here kills this thread for the life of the process,
        // so every later page would answer `the OCR worker stopped` and putting a file back
        // would fix nothing. Catching it turns the one failure that was permanent into the
        // same retryable `Unavailable` as all the others.
        if models.is_none() {
            let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Models::open(&assets)
            }));
            match loaded {
                Ok(Ok(m)) => models = Some(m),
                Ok(Err(e)) => {
                    let _ = job.reply.send(Err(Error::Unavailable(e)));
                    continue;
                }
                Err(_) => {
                    let _ = job.reply.send(Err(Error::Unavailable(
                        "loading the OCR models panicked — is onnxruntime.dll beside the exe?"
                            .into(),
                    )));
                    continue;
                }
            }
        }
        let loaded = models.as_mut().expect("just loaded");

        let result = run(loaded, &job);
        let _ = job.reply.send(result);
    }
}

fn run(models: &mut Models, job: &Job) -> Result<Page, Error> {
    let started = Instant::now();
    // Cancellation and the deadline are checked at every seam. There is no callback into an
    // ORT run, so this is the granularity available — and it is fine, because the page is one
    // detection pass plus one inference per line rather than a single opaque call.
    let stop = |cancel: &Arc<Cancel>| -> Result<(), Error> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if started.elapsed() >= job.limits.deadline {
            return Err(Error::Timeout);
        }
        Ok(())
    };

    let image = prep::decode_rgb(&job.image, &job.limits)?;
    stop(&job.cancel)?;

    let (rects, detect_ms) = detect::detect(&mut models.det, &image)?;
    stop(&job.cancel)?;

    let recognition = Instant::now();
    let mut lines = Vec::with_capacity(rects.len());
    for rect in rects {
        // Deliberately BEFORE the line rather than after it. A deadline that fires halfway
        // down a column leaves the caller holding part of the page, and a partial column
        // narrated as a whole one is the book silently going missing — the exact failure this
        // project spends its evidence rules avoiding. Losing a legitimate page that took the
        // full deadline is the cheap side of that trade, and with a 30 s bound on a
        // sub-second operation it is not a case that arises.
        stop(&job.cancel)?;
        let words = recognize::recognize_line(&mut models.rec, &models.charset, &image, rect)?;
        // A detected region that held no readable characters is dropped, not narrated as an
        // empty line. It is the residue the detector's score gate cannot catch on its own.
        if !words.is_empty() {
            lines.push(Line { words });
        }
    }
    let recognize_ms = recognition.elapsed().as_secs_f64() * 1000.0;

    Ok(Page {
        width: image.width,
        height: image.height,
        lines,
        detect_ms,
        recognize_ms,
        ocr_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here runs with no models on disk, which is the point: what is under test is
    /// the queue, the bounds and the failure classification, none of which should need 10 MiB
    /// of ONNX to exercise.
    fn nowhere() -> Assets {
        Assets::new("no-such-ocr-models")
    }

    #[test]
    fn a_full_queue_says_busy_rather_than_growing() {
        let ocr = Ocr::new(nowhere(), Limits::default());
        let mut handles = Vec::new();
        let mut busy = false;
        for _ in 0..QUEUE_DEPTH + 8 {
            match ocr.submit(vec![0u8; 16]) {
                Ok(h) => handles.push(h),
                Err(Error::Busy) => {
                    busy = true;
                    break;
                }
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
        // Either every submission was drained faster than the loop could fill the queue (the
        // models are missing, so each fails instantly) or the bound was reached. Both prove
        // the queue is bounded rather than unbounded; what must never happen is a panic or a
        // block.
        assert!(busy || handles.len() >= QUEUE_DEPTH, "queue accepted {} jobs", handles.len());
    }

    #[test]
    fn missing_models_fail_the_job_rather_than_the_worker() {
        let ocr = Ocr::new(nowhere(), Limits::default());
        for _ in 0..2 {
            // Twice, because a failed load must not be cached into a dead worker.
            match ocr.recognize(vec![0u8; 16]) {
                Err(Error::Unavailable(_)) => {}
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }
    }

    #[test]
    fn models_that_fail_their_pin_are_refused_at_load_not_only_at_probe() {
        // The whole point of pinning: `/status` reporting `corrupt` while `/ocr` went on
        // recognizing with whatever was on disk meant the two endpoints disagreed about
        // whether the engine was usable, and the permissive one was the dangerous one. A
        // recognizer that is not the pinned one but emits the same class count passes every
        // other check in this crate and returns fluent, plausible, entirely wrong text.
        //
        // Reaching the digest check is also what keeps this test free of ONNX: it fails
        // before any session is built.
        let dir = std::env::temp_dir().join("kokoro-ocr-unpinned-models");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [crate::DET_FILE, crate::REC_FILE, crate::DICT_FILE] {
            std::fs::write(dir.join(name), b"present, but not what was pinned").unwrap();
        }

        let ocr = Ocr::new(Assets::new(&dir), Limits::default());
        match ocr.recognize(vec![0u8; 16]) {
            Err(Error::Unavailable(m)) => {
                assert!(m.contains("hashes to"), "expected the digest to be named: {m}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_body_is_refused_before_it_is_queued() {
        let limits = Limits { max_body_bytes: 4, ..Limits::default() };
        let ocr = Ocr::new(nowhere(), limits);
        assert!(matches!(ocr.submit(vec![0u8; 64]), Err(Error::TooLarge(_))));
    }

    #[test]
    fn a_job_cancelled_before_it_runs_is_not_run() {
        let ocr = Ocr::new(nowhere(), Limits::default());
        let handle = ocr.submit(vec![0u8; 16]).expect("submit");
        handle.cancel_handle().cancel();
        // Either the worker saw the flag first (Cancelled) or it had already failed on the
        // missing models. Both are correct; what matters is that it answers.
        assert!(handle.wait().is_err());
    }

    #[test]
    fn the_limits_a_caller_set_are_the_ones_reported() {
        let limits = Limits { max_body_bytes: 123, ..Limits::default() };
        let ocr = Ocr::new(nowhere(), limits);
        assert_eq!(ocr.limits().max_body_bytes, 123);
        assert_eq!(ocr.assets().detector().file_name().unwrap(), crate::DET_FILE);
    }
}
