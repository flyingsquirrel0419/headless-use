//! Dewiggle: reverse per-character vertical wobble in animated text CAPTCHAs
//! using *captured pixels only*. No answer arrays, no Vue/Nuxt props, no DOM
//! secret extraction — this is an honest pixel-only approach.
//!
//! ## How it works
//! A "wiggle" CAPTCHA renders each glyph along an independent vertical sine.
//! The text is animated frame-to-frame, so a single screenshot is a snapshot
//! of many glyphs each at a different phase of their wobble. A vision model
//! reading one frame sees glyphs shifted up/down from their neutral baseline,
//! which looks like distortion (and reads unreliably).
//!
//! We exploit that the wobble is vertical and (mostly) independent per glyph
//! column-band. For each frame, for each column, we compute the vertical ink
//! centroid (the intensity-weighted y of the dark pixels). The centroid
//! oscillates around a neutral y. Averaging the centroid across frames gives
//! the neutral baseline for that column; the per-frame deviation is the wobble
//! amplitude. We then shift each frame's rows so every column's centroid lands
//! on its neutral baseline, and average the realigned frames. Ink that is at a
//! fixed location (the glyph body, once realigned) accumulates; background and
//! anti-aliasing noise averages toward gray, sharpening the glyph.
//!
//! ## Why not min-projection / time-freezing
//! - min over frames: wobble makes the glyph smear vertically (every phase
//!   sampled), which hurts OCR more than the original.
//! - Freezing performance.now(): only helps if the wobble is purely time-driven
//!   and the renderer reads the frozen clock once per glyph. In practice many
//!   wiggle renderers key off the rAF timestamp we cannot freeze without
//!   patching the page, which violates the don't-mutate-the-page rule.
//!
//! ## Honesty boundary
//! This module never reads text content, answer arrays, component props, or
//! any DOM state other than the geometric bounding box of the capture region
//! (so we know what to clip). All glyph recovery is from raw pixel intensity.

use image::GrayImage;

/// A captured frame of the wiggling text region, already converted to a
/// grayscale buffer with the same width/height as every other frame.
#[derive(Clone)]
pub struct DewiggleFrame {
    /// Grayscale pixels, row-major, length = width*height.
    pub gray: GrayImage,
}

impl DewiggleFrame {
    /// Build a frame from a decoded grayscale image.
    pub fn new(gray: GrayImage) -> Self {
        Self { gray }
    }

    /// Dimensions (width, height).
    pub fn dims(&self) -> (u32, u32) {
        self.gray.dimensions()
    }
}

/// Output of a dewiggle run: a realigned, averaged image plus per-glyph crops.
#[derive(Clone)]
pub struct DewiggleResult {
    /// The full realigned + averaged grayscale image.
    pub image: GrayImage,
    /// Optional per-glyph crops (only when chars is Some and bands were
    /// detected). Each crop is realigned independently.
    pub char_crops: Vec<GrayImage>,
    /// Detected glyph column-band boundaries (start inclusive, end exclusive).
    /// Empty when no segmentation was requested.
    pub bands: Vec<(u32, u32)>,
    /// Number of frames consumed.
    pub frame_count: usize,
}

/// Dewiggle options.
#[derive(Clone, Debug)]
pub struct DewiggleOptions {
    /// Ink threshold: pixels darker than this (0=black, 255=white) count as
    /// ink for centroid computation. Lower = only darkest ink; higher =
    /// include anti-aliased edges.
    pub ink_threshold: u8,
    /// Maximum vertical shift (px) allowed per column per frame. Guards against
    /// centroid spikes on sparse columns. 0 disables the cap.
    pub max_shift: i32,
    /// When Some(n), segment into exactly n equal-width glyph bands and also
    /// produce per-glyph crops. When None, only the global realignment is done
    /// and char_crops is empty.
    pub chars: Option<usize>,
}

impl Default for DewiggleOptions {
    fn default() -> Self {
        Self {
            ink_threshold: 128,
            max_shift: 24,
            chars: None,
        }
    }
}

/// Run dewiggle on a stack of grayscale frames.
///
/// All frames must share the same dimensions. If zero frames are given, an
/// empty result is returned. If one frame is given, it is returned unchanged
/// (no wobble to reverse) — the honest fallback for static text.
pub fn dewiggle(frames: &[DewiggleFrame], opts: &DewiggleOptions) -> DewiggleResult {
    if frames.is_empty() {
        return DewiggleResult {
            image: GrayImage::new(1, 1),
            char_crops: Vec::new(),
            bands: Vec::new(),
            frame_count: 0,
        };
    }
    let (w, h) = frames[0].dims();
    // Single frame: nothing to realign. Return as-is.
    if frames.len() == 1 {
        return DewiggleResult {
            image: frames[0].gray.clone(),
            char_crops: Vec::new(),
            bands: Vec::new(),
            frame_count: 1,
        };
    }

    // Per-column vertical ink centroid per frame:
    //   centroid_xf = (sum_y y*ink(x,y)) / (sum_y ink(x,y))  for frame f
    // Accumulate over frames to get the neutral baseline per column:
    //   neutral_x = mean_f centroid_xf
    // Then per frame the shift for column x is round(neutral_x - centroid_xf).
    let n = frames.len();
    let mut neutral = vec![0.0f64; w as usize];
    let mut col_centroids = vec![0.0f64; (w as usize) * n];
    for (fi, frame) in frames.iter().enumerate() {
        let (fw, fh) = frame.dims();
        if fw != w || fh != h {
            continue;
        }
        let px = frame.gray.as_raw();
        for x in 0..w as usize {
            let mut sum_y = 0.0f64;
            let mut sum = 0.0f64;
            for y in 0..h as usize {
                let v = px[y * w as usize + x];
                if v < opts.ink_threshold {
                    let weight = (255 - v) as f64;
                    sum_y += (y as f64) * weight;
                    sum += weight;
                }
            }
            let c = if sum > 0.0 {
                sum_y / sum
            } else {
                (h as f64) / 2.0
            };
            col_centroids[fi * w as usize + x] = c;
            neutral[x] += c;
        }
    }
    for v in neutral.iter_mut() {
        *v /= n as f64;
    }

    // Realign + average: shift each column so its centroid lands on neutral[x],
    // then accumulate pixel intensities into an accumulator buffer.
    let mut acc = vec![0.0f64; (w as usize) * (h as usize)];
    for (fi, frame) in frames.iter().enumerate() {
        let (fw, fh) = frame.dims();
        if fw != w || fh != h {
            continue;
        }
        let px = frame.gray.as_raw();
        for x in 0..w as usize {
            let c = col_centroids[fi * w as usize + x];
            let mut dy = neutral[x] - c;
            if opts.max_shift > 0 {
                let cap = opts.max_shift as f64;
                if dy > cap {
                    dy = cap;
                } else if dy < -cap {
                    dy = -cap;
                }
            }
            let dy_i = dy.round() as i32;
            for y in 0..h as i32 {
                let src_y = y - dy_i;
                if src_y < 0 || src_y >= h as i32 {
                    continue;
                }
                let v = px[(src_y as usize) * w as usize + x] as f64;
                acc[(y as usize) * w as usize + x] += v;
            }
        }
    }
    let mut out = GrayImage::new(w, h);
    let out_px = out.as_mut();
    for i in 0..acc.len() {
        let avg = (acc[i] / n as f64).round().clamp(0.0, 255.0) as u8;
        out_px[i] = avg;
    }

    // Optional per-glyph segmentation.
    let (bands, char_crops) = if let Some(nchars) = opts.chars {
        let bands = even_bands(w, nchars);
        let crops: Vec<GrayImage> = bands
            .iter()
            .map(|(x0, x1)| crop_band(&out, *x0, *x1))
            .collect();
        (bands, crops)
    } else {
        (Vec::new(), Vec::new())
    };

    DewiggleResult {
        image: out,
        char_crops,
        bands,
        frame_count: n,
    }
}

/// Split width into n contiguous equal-ish bands, returning (start, end)
/// pixel ranges (end exclusive).
fn even_bands(width: u32, n: usize) -> Vec<(u32, u32)> {
    if n == 0 || width == 0 {
        return Vec::new();
    }
    let bw = width as usize / n;
    let mut bands = Vec::with_capacity(n);
    let mut x = 0u32;
    for i in 0..n {
        let end = if i == n - 1 {
            width
        } else {
            (x + bw as u32).min(width)
        };
        bands.push((x, end));
        x = end;
    }
    bands
}

/// Crop columns [x0, x1) from an image, keeping full height.
fn crop_band(img: &GrayImage, x0: u32, x1: u32) -> GrayImage {
    let (w, h) = img.dimensions();
    let x0 = x0.min(w);
    let x1 = x1.min(w).max(x0);
    let cw = x1 - x0;
    let mut out = GrayImage::new(cw, h);
    let src = img.as_raw();
    let dst = out.as_mut();
    for y in 0..h as usize {
        for x in 0..cw as usize {
            dst[y * cw as usize + x] = src[y * w as usize + (x0 as usize) + x];
        }
    }
    out
}

/// Encode a grayscale image to PNG bytes.
pub fn encode_png(img: &GrayImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img.clone())
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {e}"))?;
    Ok(buf.into_inner())
}

/// Decode PNG/JPEG bytes into a grayscale image.
pub fn decode_gray(bytes: &[u8]) -> Result<GrayImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode: {e}"))?;
    Ok(img.to_luma8())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_frame(w: u32, h: u32, shifts: &[i32], ink_val: u8) -> GrayImage {
        // Draw a vertical bar of ink at the middle column of each glyph band,
        // shifted vertically by shifts per band. Background is white (255).
        let mut g = GrayImage::from_pixel(w, h, image::Luma([255]));
        let n = shifts.len();
        let bw = w as usize / n;
        for (bi, &dy) in shifts.iter().enumerate() {
            let cx = (bi * bw + bw / 2) as u32;
            let mid = (h as i32 / 2) + dy;
            for y in -10..=10 {
                let yy = mid + y;
                if yy >= 0 && yy < h as i32 {
                    g.put_pixel(cx, yy as u32, image::Luma([ink_val]));
                }
            }
        }
        g
    }

    #[test]
    fn empty_frames_returns_empty() {
        let r = dewiggle(&[], &DewiggleOptions::default());
        assert_eq!(r.frame_count, 0);
        assert_eq!(r.image.dimensions(), (1, 1));
    }

    #[test]
    fn single_frame_passes_through() {
        let g = synth_frame(60, 40, &[0, 0, 0], 10);
        let f = DewiggleFrame::new(g.clone());
        let r = dewiggle(&[f], &DewiggleOptions::default());
        assert_eq!(r.frame_count, 1);
        assert_eq!(r.image.as_raw(), g.as_raw());
    }

    #[test]
    fn realignment_reduces_vertical_spread() {
        // Three glyph bands, each wobbled by a different amount across frames.
        let w = 90u32;
        let h = 60u32;
        let frames: Vec<DewiggleFrame> = vec![
            DewiggleFrame::new(synth_frame(w, h, &[-10, 5, 12], 5)),
            DewiggleFrame::new(synth_frame(w, h, &[0, 0, 0], 5)),
            DewiggleFrame::new(synth_frame(w, h, &[10, -5, -12], 5)),
        ];
        let opts = DewiggleOptions {
            ink_threshold: 128,
            max_shift: 24,
            chars: Some(3),
        };
        let r = dewiggle(&frames, &opts);
        assert_eq!(r.frame_count, 3);
        assert_eq!(r.bands.len(), 3);
        assert_eq!(r.char_crops.len(), 3);

        // After realignment all three ink bars stack on the midline, so the
        // ink spread in the middle band stays near the bar height (~21px).
        let mid = &r.char_crops[1];
        let mut ys: Vec<i32> = vec![];
        for y in 0..h as i32 {
            for x in 0..mid.width() as i32 {
                if mid.get_pixel(x as u32, y as u32).0[0] < 128 {
                    ys.push(y);
                }
            }
        }
        let spread = ys.iter().max().unwrap_or(&0) - ys.iter().min().unwrap_or(&0);
        assert!(spread <= 24, "dewiggled spread too large: {spread}");
    }

    #[test]
    fn uniform_dimensions_required() {
        let a = DewiggleFrame::new(GrayImage::new(40, 40));
        let b = DewiggleFrame::new(GrayImage::new(40, 40));
        let r = dewiggle(&[a, b], &DewiggleOptions::default());
        assert_eq!(r.frame_count, 2);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let g = synth_frame(30, 20, &[0], 0);
        let png = encode_png(&g).unwrap();
        assert!(png.len() > 8);
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        let back = decode_gray(&png).unwrap();
        assert_eq!(back.dimensions(), g.dimensions());
    }

    #[test]
    fn even_bands_basic() {
        let bands = even_bands(90, 3);
        assert_eq!(bands, vec![(0, 30), (30, 60), (60, 90)]);
        let bands = even_bands(100, 3);
        assert_eq!(bands, vec![(0, 33), (33, 66), (66, 100)]);
    }

    #[test]
    fn even_bands_zero() {
        assert!(even_bands(90, 0).is_empty());
        assert!(even_bands(0, 3).is_empty());
    }
}
