// Portions of this file are ported/derived from PaddleOCR's DBNet post-processing
// (https://github.com/PaddlePaddle/PaddleOCR), licensed under the Apache License,
// Version 2.0. Copyright (c) 2020 PaddlePaddle Authors. All Rights Reserved.
//
// Modified for Kokoro Kindle Reader: connected components and axis-aligned boxes replace
// contour fitting and a Vatti polygon offset (see below for why the two agree on this
// input). Full licence text: licenses/Apache-2.0.txt. See THIRD_PARTY_NOTICES.md for the
// complete list of files this notice covers.

// Stage one: where is there text?
//
// The detector is DBNet — a segmentation network that emits one probability per pixel of a
// downscaled page. Everything below turns that map into line rectangles: threshold, connected
// components, a score gate, and an outward expansion to recover the margin the segmentation
// shrinks away.
//
// THIS IS A SIMPLIFICATION OF PADDLEOCR'S OWN POST-PROCESSING, and it is recorded rather than
// hidden. Upstream takes contours, fits a minimum-area rectangle to each, and offsets the
// polygon with a Vatti clip; this takes connected components and axis-aligned boxes with the
// equivalent offset for a rectangle. For horizontal book text set on a page the two agree —
// the fitted rectangle IS the axis-aligned one when nothing is rotated. A skewed capture is
// where they part company, and that is the first thing to revisit if a real page comes back
// wrong — including a detector that reads worse than expected, since the shortcut is here and
// not in the weights.

use std::borrow::Cow;
use std::time::Instant;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::prep::Rgb;
use crate::{Error, Rect};

/// The longest side the detector ever sees. A larger page is scaled down to fit; a smaller one
/// is left alone rather than upscaled, since recognition rescales each line from the SOURCE
/// pixels anyway and nothing is gained by interpolating twice.
///
/// 960 is PaddleOCR's own `det_limit_side_len` default with `limit_type='max'`, which is the
/// configuration these weights were exported and evaluated under.
const MAX_SIDE: u32 = 960;

/// The network's stride: both input sides must be a multiple of this.
const STRIDE: u32 = 32;

/// ImageNet normalization — the values PaddleOCR's `NormalizeImage` uses for detection.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// A pixel is text above this probability. PaddleOCR's `det_db_thresh`.
const MAP_THRESH: f32 = 0.3;

/// ...and a region is kept only if it averages this. PaddleOCR's `det_db_box_thresh`.
///
/// The two are not redundant. The first decides the SHAPE of a region, the second decides
/// whether the region is text at all — which is exactly the gate a picture-book page needs,
/// where a high-contrast edge in the artwork will cross the pixel threshold in patches
/// without ever averaging like a line of type.
const BOX_THRESH: f32 = 0.6;

/// How far a kept box is grown, as PaddleOCR's `det_db_unclip_ratio`.
///
/// Segmentation deliberately shrinks each text region so neighbouring lines do not merge, so
/// something has to give the margin back or every crop clips its own ascenders and descenders.
/// 1.7 is the ratio the probe measured this pair with; the offset distance for a rectangle is
/// `area * ratio / perimeter`, which is the Vatti offset upstream applies to the polygon.
const UNCLIP: f32 = 1.7;

/// Boxes thinner than this in either direction are not lines. PaddleOCR's `min_size`.
const MIN_SIZE: i32 = 3;

/// Two boxes are on the same line if their tops differ by less than this share of a typical
/// line's height.
///
/// A FRACTION of the page's own median box height, not a pixel count. Reading order is a rule
/// that can silently reorder the book, so it has to run on evidence the page provides rather
/// than on an assumed zoom: upstream's fixed 10 px is right at one capture size and wrong at
/// every other, and the reader controls that size through the viewport.
const SAME_LINE_FRAC: f32 = 0.5;

/// Run the detector and return line rectangles in SOURCE coordinates, in reading order.
pub fn detect(session: &mut Session, image: &Rgb) -> Result<(Vec<Rect>, f64), Error> {
    let (dw, dh) = input_size(image.width, image.height);
    let resized = image.resize(dw, dh);

    let started = Instant::now();
    let map = run(session, &resized)?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;

    let boxes = boxes_from(&map, dw, dh);
    let mut rects: Vec<Rect> = boxes
        .into_iter()
        .map(|r| to_source(r, dw, dh, image.width, image.height))
        .filter(|r| !r.is_empty())
        .collect();
    sort_reading_order(&mut rects);
    Ok((rects, elapsed))
}

/// The size the detector is fed: the image scaled to fit `MAX_SIDE`, then each side rounded to
/// a multiple of `STRIDE`.
fn input_size(w: u32, h: u32) -> (u32, u32) {
    let longest = w.max(h) as f32;
    let ratio = if longest > MAX_SIDE as f32 { MAX_SIDE as f32 / longest } else { 1.0 };
    let round = |v: u32| {
        let scaled = (v as f32 * ratio / STRIDE as f32).round() as u32;
        scaled.max(1) * STRIDE
    };
    (round(w), round(h))
}

/// One probability per pixel of the resized image.
fn run(session: &mut Session, image: &Rgb) -> Result<Vec<f32>, Error> {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut data = vec![0f32; 3 * w * h];
    // NCHW: all of red, then all of green, then all of blue.
    for (i, px) in image.pixels.chunks_exact(3).enumerate() {
        for c in 0..3 {
            data[c * w * h + i] = (f32::from(px[c]) / 255.0 - MEAN[c]) / STD[c];
        }
    }

    let tensor = Tensor::from_array((vec![1i64, 3, h as i64, w as i64], data))
        .map_err(|e| Error::Recognize(format!("detector input: {e}")))?;
    // By the graph's own name. Both PP-OCR models take exactly one input and produce exactly
    // one output, so there is nothing to choose between — but asking the session beats
    // hardcoding `"x"` and finding out from a silent shape error if an export renames it.
    let name = input_name(session, "detector")?;
    let feeds: Vec<(Cow<str>, SessionInputValue)> =
        vec![(Cow::Owned(name), SessionInputValue::from(tensor))];

    let outputs = session.run(feeds).map_err(|e| Error::Recognize(format!("detector: {e}")))?;
    let (shape, values) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| Error::Recognize(format!("detector output: {e}")))?;

    if values.len() != w * h {
        return Err(Error::Recognize(format!(
            "detector returned {:?} ({} values) for a {w}x{h} input",
            shape,
            values.len()
        )));
    }
    Ok(values.to_vec())
}

pub fn input_name(session: &Session, which: &str) -> Result<String, Error> {
    session
        .inputs()
        .first()
        .map(|i| i.name().to_string())
        .ok_or_else(|| Error::Unavailable(format!("the {which} graph declares no inputs")))
}

/// Threshold, label, score, unclip — the whole DB post-process, in the detector's own
/// coordinate space.
fn boxes_from(map: &[f32], w: u32, h: u32) -> Vec<Rect> {
    let (iw, ih) = (w as usize, h as usize);
    let mut seen = vec![false; map.len()];
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..map.len() {
        if seen[start] || map[start] <= MAP_THRESH {
            continue;
        }
        // Flood fill, 8-connected — the connectivity `cv2.findContours` uses for the
        // foreground, so a diagonal join between two strokes keeps them one region.
        let (mut x0, mut y0, mut x1, mut y1) = (iw, ih, 0usize, 0usize);
        seen[start] = true;
        stack.push(start);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % iw, i / iw);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= iw as i32 || ny >= ih as i32 {
                        continue;
                    }
                    let n = ny as usize * iw + nx as usize;
                    if !seen[n] && map[n] > MAP_THRESH {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }

        let rect = Rect { x0: x0 as i32, y0: y0 as i32, x1: x1 as i32 + 1, y1: y1 as i32 + 1 };
        if rect.width() < MIN_SIZE || rect.height() < MIN_SIZE {
            continue;
        }
        if mean_score(map, iw, rect) < BOX_THRESH {
            continue;
        }
        out.push(unclip(rect).clamped(w as i32, h as i32));
    }
    out
}

/// The mean probability over a box — PaddleOCR's `box_score_fast`, whose mask is the whole
/// rectangle once the region is axis-aligned.
///
/// Over the BOX and not over the component's own pixels. Every pixel of the component is
/// above the threshold by definition, so scoring those would score 1.0 for everything and
/// throw the gate away; what tells a line of type from a hard edge in an illustration is how
/// solidly the region is filled.
fn mean_score(map: &[f32], stride: usize, r: Rect) -> f32 {
    let mut sum = 0f32;
    let mut n = 0u32;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            sum += map[y as usize * stride + x as usize];
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// Give back the margin segmentation shrank away.
fn unclip(r: Rect) -> Rect {
    let (w, h) = (r.width() as f32, r.height() as f32);
    let perimeter = 2.0 * (w + h);
    if perimeter <= 0.0 {
        return r;
    }
    let distance = (w * h * UNCLIP / perimeter).round() as i32;
    Rect {
        x0: r.x0 - distance,
        y0: r.y0 - distance,
        x1: r.x1 + distance,
        y1: r.y1 + distance,
    }
}

/// Detector coordinates -> the submitted image's coordinates.
///
/// Rounded OUTWARD — floor the near edges, ceil the far ones — so the returned box still
/// covers every pixel the detected box covered. Rounding both edges the same way shrinks a box
/// by up to a pixel per side, and these boxes are what the extension's `hasOutlierGap`
/// measures word spacing with; systematically widening the gaps between words is how a page of
/// justified body text starts looking like a running head. It also matters to the crop: a
/// line clipped by a pixel loses the top of its ascenders.
fn to_source(r: Rect, dw: u32, dh: u32, sw: u32, sh: u32) -> Rect {
    let (fx, fy) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    Rect {
        x0: (r.x0 as f32 * fx).floor() as i32,
        y0: (r.y0 as f32 * fy).floor() as i32,
        x1: (r.x1 as f32 * fx).ceil() as i32,
        y1: (r.y1 as f32 * fy).ceil() as i32,
    }
    .clamped(sw as i32, sh as i32)
}

/// Top to bottom, then left to right within a line.
fn sort_reading_order(rects: &mut [Rect]) {
    let band = same_line_band(rects);
    rects.sort_by_key(|r| (r.y0, r.x0));
    // One stable pass of adjacent swaps, the way PaddleOCR's `sorted_boxes` does it: two
    // boxes side by side on one line can have tops a few pixels apart, and sorting on `y0`
    // alone would then read a two-part heading right to left.
    for i in 1..rects.len() {
        for j in (0..i).rev() {
            if (rects[j + 1].y0 - rects[j].y0).abs() < band && rects[j + 1].x0 < rects[j].x0 {
                rects.swap(j, j + 1);
            } else {
                break;
            }
        }
    }
}

/// Half the median box height, floored at one pixel.
fn same_line_band(rects: &[Rect]) -> i32 {
    if rects.is_empty() {
        return 1;
    }
    let mut heights: Vec<i32> = rects.iter().map(|r| r.height()).collect();
    heights.sort_unstable();
    let median = heights[heights.len() / 2];
    ((median as f32 * SAME_LINE_FRAC) as i32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_is_a_multiple_of_the_stride_on_both_sides() {
        for (w, h) in [(1194u32, 1681u32), (37, 41), (2400, 300), (960, 960)] {
            let (dw, dh) = input_size(w, h);
            assert_eq!(dw % STRIDE, 0, "{w}x{h} -> {dw}x{dh}");
            assert_eq!(dh % STRIDE, 0, "{w}x{h} -> {dw}x{dh}");
            assert!(dw >= STRIDE && dh >= STRIDE);
        }
    }

    #[test]
    fn a_large_page_is_scaled_to_fit_the_longest_side() {
        let (dw, dh) = input_size(1200, 1700);
        assert!(dh <= MAX_SIDE + STRIDE, "{dh}");
        // Aspect is preserved to within the rounding the stride forces.
        let want = 1200.0 * (dh as f32 / 1700.0);
        assert!((dw as f32 - want).abs() <= STRIDE as f32, "{dw} vs {want}");
    }

    #[test]
    fn a_small_page_is_never_upscaled() {
        // Recognition rescales each line from the source pixels; enlarging here would
        // interpolate twice and quadruple the detector's work for it.
        let (dw, dh) = input_size(300, 200);
        assert!(dw <= 320 && dh <= 224, "{dw}x{dh}");
    }

    /// A probability map with one solid rectangle of text on it.
    fn map_with(w: usize, h: usize, r: Rect, value: f32) -> Vec<f32> {
        let mut map = vec![0.0; w * h];
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                map[y as usize * w + x as usize] = value;
            }
        }
        map
    }

    #[test]
    fn a_solid_region_becomes_one_box() {
        let region = Rect { x0: 10, y0: 10, x1: 40, y1: 20 };
        let map = map_with(64, 64, region, 0.9);
        let boxes = boxes_from(&map, 64, 64);
        assert_eq!(boxes.len(), 1, "{boxes:?}");
        // Unclipped outward, so it covers the region with room to spare.
        assert!(boxes[0].x0 <= region.x0 && boxes[0].x1 >= region.x1, "{:?}", boxes[0]);
    }

    #[test]
    fn a_faint_region_is_not_text() {
        // Above the pixel threshold everywhere, but nowhere near solid enough to be a line.
        let map = map_with(64, 64, Rect { x0: 10, y0: 10, x1: 40, y1: 20 }, 0.35);
        assert!(boxes_from(&map, 64, 64).is_empty());
    }

    #[test]
    fn a_hairline_is_not_a_line_of_text() {
        let map = map_with(64, 64, Rect { x0: 5, y0: 5, x1: 50, y1: 6 }, 0.99);
        assert!(boxes_from(&map, 64, 64).is_empty());
    }

    #[test]
    fn two_separated_regions_stay_separate() {
        // The whole reason the detector is here: a running head and a folio on one band must
        // arrive as two lines, or `repeatsAcrossPages` can never match the head.
        let mut map = map_with(128, 64, Rect { x0: 4, y0: 10, x1: 40, y1: 22 }, 0.95);
        for (i, v) in map_with(128, 64, Rect { x0: 90, y0: 10, x1: 110, y1: 22 }, 0.95)
            .into_iter()
            .enumerate()
        {
            map[i] = map[i].max(v);
        }
        assert_eq!(boxes_from(&map, 128, 64).len(), 2);
    }

    #[test]
    fn boxes_never_leave_the_image() {
        let map = map_with(64, 64, Rect { x0: 0, y0: 0, x1: 20, y1: 12 }, 0.99);
        for b in boxes_from(&map, 64, 64) {
            assert!(b.x0 >= 0 && b.y0 >= 0 && b.x1 <= 64 && b.y1 <= 64, "{b:?}");
        }
    }

    #[test]
    fn unclip_grows_a_box_on_every_side() {
        let r = Rect { x0: 100, y0: 100, x1: 200, y1: 120 };
        let u = unclip(r);
        assert!(u.x0 < r.x0 && u.y0 < r.y0 && u.x1 > r.x1 && u.y1 > r.y1, "{u:?}");
    }

    #[test]
    fn source_mapping_covers_the_detected_box() {
        // A box on a half-size detector map must come back covering twice the pixels.
        let r = to_source(Rect { x0: 5, y0: 7, x1: 15, y1: 12 }, 100, 100, 200, 200);
        assert!(r.x0 <= 10 && r.x1 >= 30 && r.y0 <= 14 && r.y1 >= 24, "{r:?}");
    }

    fn line(y: i32, x: i32) -> Rect {
        Rect { x0: x, y0: y, x1: x + 50, y1: y + 20 }
    }

    #[test]
    fn reading_order_is_top_to_bottom_then_left_to_right() {
        let mut rects = vec![line(100, 200), line(40, 10), line(102, 10), line(40, 300)];
        sort_reading_order(&mut rects);
        assert_eq!(
            rects.iter().map(|r| (r.y0, r.x0)).collect::<Vec<_>>(),
            vec![(40, 10), (40, 300), (102, 10), (100, 200)]
        );
    }

    #[test]
    fn the_same_line_band_scales_with_the_type() {
        // Two boxes 8 px apart are one line of 40 px type and two lines of 6 px type. A fixed
        // pixel threshold has to be wrong about one of them.
        let big = vec![Rect { x0: 0, y0: 0, x1: 50, y1: 40 }; 4];
        let small = vec![Rect { x0: 0, y0: 0, x1: 50, y1: 6 }; 4];
        assert!(same_line_band(&big) > 8);
        assert!(same_line_band(&small) < 8);
    }

    #[test]
    fn ordering_an_empty_page_is_not_a_panic() {
        let mut rects: Vec<Rect> = Vec::new();
        sort_reading_order(&mut rects);
        assert!(rects.is_empty());
    }
}
