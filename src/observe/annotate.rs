//! Screenshot annotator: draws bounding boxes + ref labels over a PNG.
//!
//! [Decision Log]
//! - 목적과 의도: screenshot --annotate 가 요소의 ref/좌표를 PNG 위에
//!   그려 비전 기반 에이전트가 정확한 클릭 좌표를 시각적으로 확인하게 함.
//! - 기존 구현 및 제약 조건: 최초 구현은 "의존성을 늘리지 않는다"를 이유로
//!   PNG 컨테이너/IHDR/필터 역변환/청크 CRC를 직접 구현하고 zlib만 flate2에
//!   맡겼다. 그런데 바로 다음 기능 커밋(dewiggle)이 `image` 크레이트를 png
//!   기능과 함께 추가해 버렸고, 직접 구현을 정당화하던 근거는 그때 사라졌다.
//!   그 뒤로 같은 바이너리 안에 PNG 디코더가 둘 링크되어 있었다.
//! - 검토한 주요 대안: 직접 구현 유지(현상 유지), dewiggle 쪽을 직접 구현으로
//!   되돌리기.
//! - 선택한 방식: 이미 의존하고 있는 `image` 크레이트로 디코딩/인코딩을
//!   넘기고 직접 구현한 코덱(약 224줄)을 삭제. 그 결과 flate2 의존성도 함께
//!   제거됐다.
//! - 다른 대안 대신 이 방식을 선택한 이유: 검증된 코덱 하나가 자체 구현
//!   서브셋보다 안전하고, 의존성 총량은 오히려 줄어든다.
//! - 장점, 단점 및 영향: 인터레이스/16비트 PNG까지 자연히 지원된다. 모든
//!   이미지가 RGBA로 정규화되므로 출력은 항상 RGBA PNG이며, RGB 입력에서도
//!   라벨 배경의 알파가 실제로 블렌딩된다(이전에는 무시하고 덮어썼다).

use image::{ImageFormat, RgbaImage};

use crate::cdp::Viewport;
use crate::observe::ElementRef;

/// Decode a PNG into an 8-bit RGBA image.
///
/// Everything is normalized to RGBA so the drawing code has one pixel layout to
/// handle instead of branching on channel count.
pub fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    let img = image::load_from_memory_with_format(bytes, ImageFormat::Png)
        .map_err(|e| format!("decode png: {e}"))?;
    Ok(img.to_rgba8())
}

/// Encode an RGBA image as a PNG.
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("encode png: {e}"))?;
    Ok(buf.into_inner())
}

/// Blend `color` over the pixel at (x, y), ignoring out-of-bounds coordinates.
///
/// Source-over with a straight (non-premultiplied) alpha, applied to all four
/// channels — the same arithmetic the previous hand-rolled surface used, so
/// annotated output is unchanged for the RGBA screenshots CDP actually returns.
fn put_pixel(img: &mut RgbaImage, x: i64, y: i64, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return;
    }
    let px = img.get_pixel_mut(x as u32, y as u32);
    let a = color[3] as u32;
    for (dst, &new) in px.0.iter_mut().zip(color.iter()) {
        let cur = *dst as u32;
        *dst = ((new as u32 * a + cur * (255 - a)) / 255) as u8;
    }
}

/// Draw a 1px-thick rectangle outline at (x, y, w, h).
fn draw_rect(img: &mut RgbaImage, x: i64, y: i64, w: i64, h: i64, color: [u8; 4]) {
    let x2 = x + w;
    let y2 = y + h;
    for dx in 0..=w {
        put_pixel(img, x + dx, y, color);
        put_pixel(img, x + dx, y2, color);
    }
    for dy in 0..=h {
        put_pixel(img, x, y + dy, color);
        put_pixel(img, x2, y + dy, color);
    }
}

/// A 5x7 bitmap font for single-digit-ish labels. Each glyph is 5 wide, 7 tall.
/// Supports digits 0-9, letters A-Z (upper), and a few punctuation marks used in
/// ref tokens (`@`, `g`, `:`, `e`). Lowercase is mapped to uppercase glyphs.
static FONT: &[(&str, &[u8; 7])] = &[
    (
        "0",
        &[
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
    ),
    (
        "1",
        &[
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
    ),
    (
        "2",
        &[
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
    ),
    (
        "3",
        &[
            0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110,
        ],
    ),
    (
        "4",
        &[
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
    ),
    (
        "5",
        &[
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
    ),
    (
        "6",
        &[
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        "7",
        &[
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
    ),
    (
        "8",
        &[
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        "9",
        &[
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
    ),
    (
        "A",
        &[
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        "B",
        &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
    ),
    (
        "C",
        &[
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
    ),
    (
        "D",
        &[
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
    ),
    (
        "E",
        &[
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
    ),
    (
        "F",
        &[
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
    ),
    (
        "G",
        &[
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        "H",
        &[
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        "I",
        &[
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
    ),
    (
        "J",
        &[
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
    ),
    (
        "K",
        &[
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
    ),
    (
        "L",
        &[
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
    ),
    (
        "M",
        &[
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        "N",
        &[
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
    ),
    (
        "O",
        &[
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        "P",
        &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
    ),
    (
        "Q",
        &[
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
    ),
    (
        "R",
        &[
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
    ),
    (
        "S",
        &[
            0b01110, 0b10001, 0b10000, 0b01110, 0b00001, 0b10001, 0b01110,
        ],
    ),
    (
        "T",
        &[
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
    ),
    (
        "U",
        &[
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
    ),
    (
        "V",
        &[
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
    ),
    (
        "W",
        &[
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
    ),
    (
        "X",
        &[
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
    ),
    (
        "Y",
        &[
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
    ),
    (
        "Z",
        &[
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
    ),
    (
        "@",
        &[
            0b01110, 0b10001, 0b10111, 0b10101, 0b10111, 0b10000, 0b01110,
        ],
    ),
    (
        ":",
        &[
            0b00000, 0b00100, 0b00000, 0b00000, 0b00000, 0b00100, 0b00000,
        ],
    ),
];

/// Draw a short label at (x, y) using the 5x7 bitmap font, 1px gap between glyphs.
fn draw_label(img: &mut RgbaImage, x: i64, y: i64, text: &str, color: [u8; 4]) {
    let mut cx = x;
    for ch in text.chars() {
        let upper = ch.to_ascii_uppercase().to_string();
        if let Some((_, glyph)) = FONT.iter().find(|(c, _)| *c == upper) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    if (bits >> (4 - col)) & 1 != 0 {
                        put_pixel(img, cx + col, y + row as i64, color);
                    }
                }
            }
        }
        cx += 6; // 5px wide + 1px gap
    }
}

/// Annotate a screenshot PNG with bounding boxes and ref tokens for each
/// observed element.
///
/// Standard interactive elements get a green outline; visual widgets (canvas,
/// svg, cursor:pointer divs) get an orange outline. Each box is labeled with
/// its ref token (e.g. `@g3:e7`) so a vision agent can read the coordinates.
pub fn annotate(
    png_bytes: &[u8],
    elements: &[ElementRef],
    _viewport: &Viewport,
) -> Result<Vec<u8>, String> {
    let mut img = decode_png(png_bytes)?;
    let green = [0u8, 200, 0, 255];
    let orange = [255, 165, 0, 255];
    let label_bg = [0, 0, 0, 220];
    let label_fg = [255, 255, 255, 255];

    for el in elements {
        let color = if el.visual { orange } else { green };
        let x = el.x;
        let y = el.y;
        let w = el.width.max(1);
        let h = el.height.max(1);
        draw_rect(&mut img, x, y, w, h, color);
        // Draw the ref token label in the top-left corner of the box.
        let token = if el.ref_token.is_empty() {
            format!("@e{}", el.ref_id)
        } else {
            el.ref_token.clone()
        };
        // Compact label: drop the @ and generation for readability on tiny boxes.
        let label = token.trim_start_matches('@');
        // Label background block (rough width estimate: 6px per glyph + 4 pad).
        let lw = (label.len() as i64) * 6 + 4;
        let lh = 9;
        for dy in 0..lh {
            for dx in 0..lw {
                put_pixel(&mut img, x + dx, y + dy, label_bg);
            }
        }
        draw_label(&mut img, x + 2, y + 1, label, label_fg);
    }
    encode_png(&img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdp::Viewport;
    use crate::observe::ElementRef;

    /// Build a solid PNG. `channels` selects the *source* color type so the
    /// decoder is still exercised on RGB and grayscale inputs, even though
    /// everything is normalized to RGBA once decoded.
    fn solid_png(w: u32, h: u32, channels: u8, fill: u8) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let dynimg = match channels {
            1 => image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(
                w,
                h,
                image::Luma([fill]),
            )),
            3 => image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                w,
                h,
                image::Rgb([fill, fill, fill]),
            )),
            _ => image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                w,
                h,
                image::Rgba([fill, fill, fill, fill]),
            )),
        };
        dynimg.write_to(&mut buf, ImageFormat::Png).expect("encode");
        buf.into_inner()
    }

    /// An RGB source decodes to RGBA with an opaque alpha channel.
    #[test]
    fn png_rgb_source_decodes_to_opaque_rgba() {
        let decoded = decode_png(&solid_png(4, 3, 3, 7)).expect("decode");
        assert_eq!(decoded.width(), 4);
        assert_eq!(decoded.height(), 3);
        assert!(decoded.pixels().all(|p| p.0 == [7, 7, 7, 255]));
    }

    #[test]
    fn png_rgba_source_round_trips() {
        let decoded = decode_png(&solid_png(5, 5, 4, 9)).expect("decode");
        assert!(decoded.pixels().all(|p| p.0 == [9, 9, 9, 9]));
    }

    /// Grayscale is a color type the old hand-rolled decoder claimed to support;
    /// pinned here so the swap did not quietly drop it.
    #[test]
    fn png_grayscale_source_decodes() {
        let decoded = decode_png(&solid_png(3, 2, 1, 40)).expect("decode");
        assert!(decoded.pixels().all(|p| p.0 == [40, 40, 40, 255]));
    }

    #[test]
    fn decode_rejects_non_png() {
        assert!(decode_png(b"not a png at all").is_err());
    }

    fn el(x: i64, y: i64, w: i64, h: i64, visual: bool, id: u32) -> ElementRef {
        ElementRef {
            ref_id: id,
            role: if visual {
                "canvas".into()
            } else {
                "button".into()
            },
            name: "Go".into(),
            tag_name: if visual { "canvas" } else { "button" }.into(),
            x,
            y,
            width: w,
            height: h,
            visible: true,
            enabled: true,
            focused: false,
            checked: None,
            value: None,
            selector_hint: String::new(),
            visual,
            opaque_interactive: false,
            ref_token: format!("@g1:e{id}"),
        }
    }

    #[test]
    fn annotate_draws_pixels_inside_box_and_label() {
        // 40x40 white image; draw a box at (2,2,30,30). The ref-token label
        // occupies roughly the top-left 34x9 region of the box, so an interior
        // point below the label (y >= 12) and inside the box stays white.
        let png = solid_png(40, 40, 4, 255);
        let vp = Viewport {
            width: 40,
            height: 40,
            device_scale_factor: 1.0,
        };
        let annotated = annotate(&png, &[el(2, 2, 30, 30, false, 3)], &vp).expect("annotate");
        let img = decode_png(&annotated).expect("decode annotated");

        // The top-left corner pixel of the box should no longer be pure white
        // (the green outline + label background were drawn over it).
        let corner = pixel(&img, 2, 2);
        assert_ne!(corner, [255, 255, 255, 255], "corner should be overdrawn");

        // A pixel well inside the box, below the label band, stays white.
        let interior = pixel(&img, 20, 20);
        assert_eq!(
            interior,
            [255, 255, 255, 255],
            "interior should be untouched"
        );

        // The right-edge outline pixel (x = box right, mid-height) should be green.
        let edge = pixel(&img, 32, 17);
        assert!(
            edge[1] > edge[0] && edge[1] > edge[2],
            "edge should be green, got {edge:?}"
        );
    }

    #[test]
    fn annotate_uses_orange_for_visual_widgets() {
        let png = solid_png(40, 40, 4, 255);
        let vp = Viewport {
            width: 40,
            height: 40,
            device_scale_factor: 1.0,
        };
        let annotated = annotate(&png, &[el(5, 5, 25, 25, true, 1)], &vp).expect("annotate");
        let img = decode_png(&annotated).expect("decode annotated");
        // right edge of the box (x = box right, mid-height) should be orange.
        let edge = pixel(&img, 30, 17);
        assert!(
            edge[0] > 150 && edge[1] > 100 && edge[2] < 80,
            "expected orange, got {edge:?}"
        );
    }

    fn pixel(img: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        img.get_pixel(x, y).0
    }
}
