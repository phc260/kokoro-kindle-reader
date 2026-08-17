// Portions of this file are ported/derived from PaddleOCR's recognition
// post-processing (https://github.com/PaddlePaddle/PaddleOCR), licensed under the
// Apache License, Version 2.0: the normalization below and the no-batch-padding CTC
// decode follow its convention. Copyright (c) 2020 PaddlePaddle Authors. All Rights
// Reserved. Modified for this project. See THIRD_PARTY_NOTICES.md.

// Stage two: what does that line say, and where is each word?
//
// The recognizer reads one line crop left to right and emits a character distribution per
// horizontal slice — a CTC head. Two things fall out of that, and the second is the one this
// project needed:
//
//   * the TEXT, by collapsing repeats and dropping the blank class, and
//   * the WORD BOXES, because the timestep at which a character fires IS its x-position.
//     Detection returns line boxes, but `hasOutlierGap` measures the gap between consecutive
//     WORDS — so an engine that could not produce word boxes could not drive the extension's
//     furniture policy at all, and the highlight would have nothing to draw. The boxes were
//     already in the model's output; they were only being thrown away. (Same move as
//     `model_patch.rs` on the Kokoro graph.)
//
// Sub-character accurate against an independent engine's own boxes, and measured rather than
// assumed. `a_words_box_spans_the_timesteps_that_produced_it` is what holds the mapping in place.
//
// Every word gets its LINE's vertical extent rather than a tight per-glyph box. Deliberate,
// and if anything the better shape for a highlight drawn over a page image: nothing in the
// policy reads y at word level, and a box that hugged the x-height would leave the ascenders
// of its own word outside the highlight.

use std::borrow::Cow;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::detect::input_name;
use crate::prep::Rgb;
use crate::{Error, Rect, Word};

/// Every line crop is resized to this height. The recognizer was trained at it, and it is why
/// this pair needs no page-wide upscaling: small type is enlarged per line, from the source
/// pixels, exactly as much as the model wants.
const REC_HEIGHT: u32 = 48;

/// A ceiling on the resized width, so one absurd detection cannot ask for an unbounded tensor.
/// A full-measure line of book text lands around 20x its height; this is 80x.
const MAX_WIDTH: u32 = REC_HEIGHT * 80;

/// Recognize one detected line. Returns its words, left to right.
///
/// An empty result is a good answer — a detected region that turned out to hold no readable
/// characters is exactly what the score gate cannot catch on its own, and the caller drops the
/// line rather than narrating an empty one.
pub fn recognize_line(
    session: &mut Session,
    charset: &[String],
    image: &Rgb,
    line: Rect,
) -> Result<Vec<Word>, Error> {
    if line.width() <= 0 || line.height() <= 0 {
        return Ok(Vec::new());
    }
    let crop = image.crop(line);
    let width = target_width(crop.width, crop.height);
    let resized = crop.resize(width, REC_HEIGHT);

    let (w, h) = (resized.width as usize, resized.height as usize);
    let mut data = vec![0f32; 3 * w * h];
    // NCHW, scaled to [-1, 1] — PaddleOCR's recognition normalization, which is a plain
    // (x/255 - 0.5) / 0.5 rather than the ImageNet statistics detection uses. The two stages
    // genuinely differ here; using one model's normalization for the other produces confident
    // nonsense, not an error.
    for (i, px) in resized.pixels.chunks_exact(3).enumerate() {
        for c in 0..3 {
            data[c * w * h + i] = f32::from(px[c]) / 127.5 - 1.0;
        }
    }

    let tensor = Tensor::from_array((vec![1i64, 3, h as i64, w as i64], data))
        .map_err(|e| Error::Recognize(format!("recognizer input: {e}")))?;
    let name = input_name(session, "recognizer")?;
    let feeds: Vec<(Cow<str>, SessionInputValue)> =
        vec![(Cow::Owned(name), SessionInputValue::from(tensor))];

    let outputs = session.run(feeds).map_err(|e| Error::Recognize(format!("recognizer: {e}")))?;
    let (shape, values) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Recognize(format!("recognizer output: {e}")))?;

    // [1, timesteps, classes]
    let (steps, classes) = match &shape[..] {
        [_, t, c] if *t >= 0 && *c > 0 => (*t as usize, *c as usize),
        other => {
            return Err(Error::Recognize(format!("recognizer returned shape {other:?}")));
        }
    };
    // One CTC blank plus the alphabet. Checked every run because the failure it catches — a
    // dictionary and a recognizer that disagree — reads out as fluent, confident, entirely
    // wrong text rather than as an error.
    if classes != charset.len() + 1 {
        return Err(Error::Unavailable(format!(
            "the recognizer emits {classes} classes but the dictionary describes {} \
             (1 blank + {} characters) — they are not a matching pair",
            charset.len() + 1,
            charset.len()
        )));
    }
    if values.len() < steps * classes {
        return Err(Error::Recognize(format!(
            "recognizer returned {} values for {steps}x{classes}",
            values.len()
        )));
    }

    Ok(decode(values, steps, classes, charset, line, crop.width))
}

/// The width the crop is resized to: whatever preserves its aspect at `REC_HEIGHT`.
///
/// No padding, because this runs one line at a time. PaddleOCR pads a BATCH out to its widest
/// member; a batch of one is its own widest member, so the padding branch is exactly zero
/// columns wide and writing it would only be a way to get it wrong.
fn target_width(w: u32, h: u32) -> u32 {
    if h == 0 {
        return REC_HEIGHT;
    }
    let want = (f64::from(w) * f64::from(REC_HEIGHT) / f64::from(h)).ceil() as u32;
    want.clamp(1, MAX_WIDTH)
}

/// Greedy CTC decode into words, carrying each character's timestep across as its x-position.
///
/// The resize cancels out of the geometry: a character at timestep `t` of `steps` sits at
/// `t / steps` of the way across the crop whatever width the crop was resized to, so the box
/// is computed straight from the SOURCE crop's width and never has to be scaled back.
fn decode(
    values: &[f32],
    steps: usize,
    classes: usize,
    charset: &[String],
    line: Rect,
    crop_width: u32,
) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut text = String::new();
    let mut confidences: Vec<f32> = Vec::new();
    let mut first_step: Option<usize> = None;
    let mut last_step = 0usize;
    let mut previous = usize::MAX;

    // Timestep -> x in the submitted image. `ceil` on the far edge for the same reason the
    // detector rounds outward: a box that is a pixel short is a box that clips its own glyph.
    let step_x0 = |t: usize| {
        line.x0 + (t as f64 * f64::from(crop_width) / steps as f64).floor() as i32
    };
    let step_x1 = |t: usize| {
        line.x0 + ((t + 1) as f64 * f64::from(crop_width) / steps as f64).ceil() as i32
    };

    let flush = |text: &mut String,
                 confidences: &mut Vec<f32>,
                 first: &mut Option<usize>,
                 last: usize,
                 words: &mut Vec<Word>| {
        let Some(start) = first.take() else {
            text.clear();
            confidences.clear();
            return;
        };
        let word = std::mem::take(text);
        let confidence = if confidences.is_empty() {
            0.0
        } else {
            confidences.iter().sum::<f32>() / confidences.len() as f32
        };
        confidences.clear();
        if word.trim().is_empty() {
            return;
        }
        words.push(Word {
            text: word,
            // 0-100, and clamped: the pinned export ends in a softmax, so these are already
            // probabilities. The clamp is so an export that ever stopped ending in one could
            // not put a value outside the contract into a field the extension averages.
            confidence: confidence.clamp(0.0, 1.0) * 100.0,
            // Inside the line by construction: the last timestep's far edge is the crop's own
            // width, and the crop was taken at `line`. Nothing to clamp.
            bbox: Rect { x0: step_x0(start), y0: line.y0, x1: step_x1(last), y1: line.y1 },
        });
    };

    for t in 0..steps {
        let row = &values[t * classes..(t + 1) * classes];
        let (class, probability) = argmax(row);

        // Collapse: a class repeated on consecutive timesteps is one character being held,
        // and class 0 is the blank that separates two genuine repeats ("ll").
        if class == previous {
            if class != 0 {
                last_step = t;
            }
            continue;
        }
        previous = class;
        if class == 0 {
            continue;
        }

        let symbol = &charset[class - 1];
        if symbol.trim().is_empty() {
            // The space class — the only thing that separates words, and the reason this
            // recognizer was chosen over one whose dictionary has no space in it.
            flush(&mut text, &mut confidences, &mut first_step, last_step, &mut words);
            continue;
        }

        if first_step.is_none() {
            first_step = Some(t);
        }
        last_step = t;
        text.push_str(symbol);
        confidences.push(probability);
    }
    flush(&mut text, &mut confidences, &mut first_step, last_step, &mut words);

    words
}

fn argmax(row: &[f32]) -> (usize, f32) {
    let mut best = 0usize;
    let mut value = f32::NEG_INFINITY;
    for (i, &v) in row.iter().enumerate() {
        if v > value {
            best = i;
            value = v;
        }
    }
    (best, if value.is_finite() { value } else { 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charset() -> Vec<String> {
        // blank is class 0; "a".."e" are 1..5; the trailing space class is 6.
        ["a", "b", "c", "d", "e", " "].iter().map(|s| s.to_string()).collect()
    }

    /// A one-hot probability row per timestep, from class indices.
    fn values(steps: &[usize], classes: usize) -> Vec<f32> {
        let mut out = vec![0.0; steps.len() * classes];
        for (t, &c) in steps.iter().enumerate() {
            out[t * classes + c] = 0.9;
        }
        out
    }

    fn line() -> Rect {
        Rect { x0: 100, y0: 200, x1: 400, y1: 240 }
    }

    fn decode_classes(steps: &[usize]) -> Vec<Word> {
        let set = charset();
        let classes = set.len() + 1;
        decode(&values(steps, classes), steps.len(), classes, &set, line(), 300)
    }

    #[test]
    fn repeats_collapse_and_the_blank_separates_a_real_double() {
        // "ab" then a blank then "bb" -> "ab" and a genuine "b".
        let words = decode_classes(&[1, 1, 2, 0, 2, 2]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "abb");
    }

    #[test]
    fn the_space_class_is_what_splits_words() {
        let words = decode_classes(&[1, 2, 6, 3, 4]);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), vec!["ab", "cd"]);
    }

    #[test]
    fn a_words_box_spans_the_timesteps_that_produced_it() {
        // 6 timesteps over a 300 px crop starting at x=100: "cd" fires at t=3,4.
        let words = decode_classes(&[1, 2, 6, 3, 4, 0]);
        let second = &words[1];
        assert!(second.bbox.x0 >= 240 && second.bbox.x0 <= 260, "{:?}", second.bbox);
        assert!(second.bbox.x1 >= 340 && second.bbox.x1 <= 360, "{:?}", second.bbox);
    }

    #[test]
    fn every_word_takes_its_lines_vertical_extent() {
        for w in decode_classes(&[1, 6, 2]) {
            assert_eq!((w.bbox.y0, w.bbox.y1), (200, 240));
        }
    }

    #[test]
    fn boxes_run_left_to_right_and_never_overlap_backwards() {
        let words = decode_classes(&[1, 6, 2, 6, 3]);
        for pair in words.windows(2) {
            assert!(pair[0].bbox.x0 < pair[1].bbox.x0, "{:?}", words);
        }
    }

    #[test]
    fn leading_and_trailing_spaces_produce_no_empty_words() {
        let words = decode_classes(&[6, 6, 1, 6, 6]);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "a");
    }

    #[test]
    fn an_all_blank_line_recognizes_nothing() {
        assert!(decode_classes(&[0, 0, 0]).is_empty());
    }

    #[test]
    fn confidence_is_reported_on_the_extensions_scale() {
        let words = decode_classes(&[1, 2]);
        assert!((words[0].confidence - 90.0).abs() < 0.01, "{}", words[0].confidence);
    }

    #[test]
    fn the_resize_preserves_the_aspect_ratio() {
        assert_eq!(target_width(300, 48), 300);
        assert_eq!(target_width(150, 24), 300);
        assert_eq!(target_width(0, 48), 1);
    }

    #[test]
    fn an_absurd_line_cannot_ask_for_an_unbounded_tensor() {
        assert_eq!(target_width(1_000_000, 4), MAX_WIDTH);
    }

    #[test]
    fn a_degenerate_crop_does_not_divide_by_zero() {
        assert_eq!(target_width(100, 0), REC_HEIGHT);
    }
}
