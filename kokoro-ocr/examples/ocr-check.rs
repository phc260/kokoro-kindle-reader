//! Run the real models over one PNG and print what came back. No browser, no host, no Kindle.
//!
//! The unit tests deliberately need no models: they cover the bounds, the DB post-processing,
//! the dictionary and the CTC decode from synthetic inputs, so they run anywhere. What none of
//! them can tell you is whether the two graphs agree with this code about tensor layout, class
//! count and coordinate space — a mistake there produces confident, fluent, entirely wrong
//! text, which is exactly the failure that reads as correct in a diff. This is the smallest
//! thing that answers that question.
//!
//! ```powershell
//! $env:ORT_DYLIB_PATH = "native-deps\runtime\onnxruntime.dll"   # the host stages this itself
//! $env:WORDS = "1"                                             # per-word boxes, not just text
//! cargo run --manifest-path kokoro-ocr\Cargo.toml --example ocr-check -- page.png native-deps\ocr
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let image = std::fs::read(&args[1]).expect("read image");
    let dir = &args[2];

    let status = kokoro_ocr::probe(&kokoro_ocr::Assets::new(dir));
    println!("probe: {:?} {:?}", status.state, status.detail);

    let ocr = kokoro_ocr::Ocr::new(kokoro_ocr::Assets::new(dir), kokoro_ocr::Limits::default());
    match ocr.recognize(image) {
        Ok(page) => {
            println!(
                "{}x{}  detect {:.1} ms  recognize {:.1} ms  total {:.1} ms  {} lines",
                page.width,
                page.height,
                page.detect_ms,
                page.recognize_ms,
                page.ocr_ms,
                page.lines.len()
            );
            for line in &page.lines {
                let text: Vec<&str> = line.words.iter().map(|w| w.text.as_str()).collect();
                println!("  {}", text.join(" "));
                if std::env::var("WORDS").is_ok() {
                    for w in &line.words {
                        println!(
                            "      {:>14}  x {:>4}..{:<4}  y {:>4}..{:<4}  {:.0}%",
                            w.text, w.bbox.x0, w.bbox.x1, w.bbox.y0, w.bbox.y1, w.confidence
                        );
                    }
                }
            }
        }
        Err(e) => println!("FAILED: {e}"),
    }
}
