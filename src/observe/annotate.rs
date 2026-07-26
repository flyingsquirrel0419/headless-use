//! Minimal PNG annotator: draws bounding boxes + ref labels over a screenshot.
//!
//! [Decision Log]
//! - 목적과 의도: screenshot --annotate 가 요소의 ref/좌표를 PNG 위에
//!   그려 비전 기반 에이전트가 정확한 클릭 좌표를 시각적으로 확인하게 함.
//! - 기존 구현 및 제약 조건: 의존성 없이 PNG를 다루려면 zlib(DEFLATE)과
//!   필터 역변환이 필요. 직접 구현은 버그 위험이 큼.
//! - 검토한 주요 대안: image crate 도입(의존성 증가), PNG 픽셀 직접 조작
//!   (zlib만 flate2로 처리).
//! - 선택한 방식: flate2(zlib)만 추가하고 PNG 컨테이너/IHDR/필터는 직접
//!   구현. CDP 스크린샷은 항상 8-bit RGBA 또는 RGB 비인터레이스 PNG.
//! - 장점: 의존성 1개 추가로 끝. 단점: PNG 서브셋만 지원(충분함).

use std::io::{Read, Write};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::cdp::Viewport;
use crate::observe::ElementRef;

/// A decoded PNG (8-bit, RGB or RGBA, non-interlaced).
pub struct PngImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// 3 (RGB) or 4 (RGBA) bytes per pixel, row-major.
    pub channels: u8,
    /// Decoded pixel buffer.
    pub data: Vec<u8>,
}

impl PngImage {
    /// Bytes per row.
    fn stride(&self) -> usize {
        self.width as usize * self.channels as usize
    }

    /// Put a pixel at (x,y), clamping to bounds. Preserves alpha if present.
    fn put_pixel(&mut self, x: i64, y: i64, color: [u8; 4]) {
        if x < 0 || y < 0 {
            return;
        }
        let xu = x as usize;
        let yu = y as usize;
        if xu >= self.width as usize || yu >= self.height as usize {
            return;
        }
        let i = yu * self.stride() + xu * self.channels as usize;
        let n = self.channels as usize;
        for c in 0..n {
            let v = if n == 4 {
                // Premultiply-ish: blend over existing with given alpha.
                let a = color[3] as u32;
                let cur = self.data[i + c] as u32;
                let new = color[c] as u32;
                ((new * a + cur * (255 - a)) / 255) as u8
            } else {
                // No alpha channel: take the RGB as-is (opaque).
                color[c]
            };
            self.data[i + c] = v;
        }
    }

    /// Draw a 1px-thick rectangle outline at (x,y,w,h).
    fn draw_rect(&mut self, x: i64, y: i64, w: i64, h: i64, color: [u8; 4]) {
        let x2 = x + w;
        let y2 = y + h;
        for dx in 0..=w {
            self.put_pixel(x + dx, y, color);
            self.put_pixel(x + dx, y2, color);
        }
        for dy in 0..=h {
            self.put_pixel(x, y + dy, color);
            self.put_pixel(x2, y + dy, color);
        }
    }
}

/// Parse a PNG chunk's length and type starting at `pos`. Returns (len, type, header_end).
fn read_chunk(buf: &[u8], pos: usize) -> Option<(u32, [u8; 4], usize)> {
    if pos + 8 > buf.len() {
        return None;
    }
    let len = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
    let ctype = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
    // Chunk data starts after the 8-byte header; CRC follows the data.
    Some((len, ctype, pos + 8))
}

/// Decode a PNG into an 8-bit image. Supports color types 2 (RGB), 6 (RGBA),
/// and 0 (grayscale) / 4 (grayscale+alpha) — the formats CDP screenshots use.
pub fn decode_png(bytes: &[u8]) -> Result<PngImage, String> {
    if bytes.len() < 8 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG".into());
    }
    let mut pos = 8;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut color_type = 0u8;
    let mut idat: Vec<u8> = Vec::new();
    let mut palette: Vec<[u8; 3]> = Vec::new();

    while let Some((len, ctype, data_start)) = read_chunk(bytes, pos) {
        let data_end = data_start + len as usize;
        if data_end + 4 > bytes.len() {
            return Err("truncated PNG chunk".into());
        }
        let data = &bytes[data_start..data_end];
        match &ctype {
            b"IHDR" => {
                if data.len() < 13 {
                    return Err("short IHDR".into());
                }
                width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
                bit_depth = data[8];
                color_type = data[9];
                if data[10] != 0 || data[11] != 0 || data[12] != 0 {
                    return Err("unsupported PNG (interlaced or custom)".into());
                }
            }
            b"PLTE" => {
                for chunk in data.chunks(3) {
                    if chunk.len() == 3 {
                        palette.push([chunk[0], chunk[1], chunk[2]]);
                    }
                }
            }
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = data_end + 4; // skip CRC
    }

    if bit_depth != 8 {
        return Err(format!("unsupported bit depth {bit_depth} (expected 8)"));
    }
    let channels: u8 = match color_type {
        0 => 1, // grayscale
        2 => 3, // RGB
        3 => 1, // palette (expanded to RGB below)
        4 => 2, // grayscale + alpha
        6 => 4, // RGBA
        _ => return Err(format!("unsupported color type {color_type}")),
    };

    // Decompress the concatenated IDAT stream.
    let mut decoder = ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::new();
    decoder
        .read_to_end(&mut raw)
        .map_err(|e| format!("zlib inflate: {e}"))?;

    // Undo PNG row filters. Each row is prefixed by a 1-byte filter type.
    let bpp = channels as usize; // bytes per pixel (8-bit)
    let stride = width as usize * bpp;
    let mut prev_row = vec![0u8; stride];
    let mut out = vec![0u8; (width as usize) * (height as usize) * bpp];

    for row in 0..height as usize {
        let off = row * (stride + 1);
        if off + 1 + stride > raw.len() {
            return Err("truncated filtered data".into());
        }
        let filter = raw[off];
        let in_row = &raw[off + 1..off + 1 + stride];
        let cur = &mut out[row * stride..row * stride + stride];
        unfilter_row(filter, bpp, in_row, &prev_row, cur);
        prev_row.copy_from_slice(cur);
    }

    // Expand palette to RGB.
    if color_type == 3 {
        let mut rgb = Vec::with_capacity((width * height) as usize * 3);
        for &b in out.iter().take((width * height) as usize) {
            let idx = b as usize;
            let p = palette.get(idx).copied().unwrap_or([0, 0, 0]);
            rgb.extend_from_slice(&p);
        }
        return Ok(PngImage {
            width,
            height,
            channels: 3,
            data: rgb,
        });
    }

    Ok(PngImage {
        width,
        height,
        channels,
        data: out,
    })
}

/// Apply a PNG row filter to reconstruct the raw scanline.
fn unfilter_row(filter: u8, bpp: usize, cur_in: &[u8], prev: &[u8], out: &mut [u8]) {
    let n = out.len();
    let paeth = |a: u8, b: u8, c: u8| -> u8 {
        let p = a as i32 + b as i32 - c as i32;
        let pa = (p - a as i32).unsigned_abs();
        let pb = (p - b as i32).unsigned_abs();
        let pc = (p - c as i32).unsigned_abs();
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    };
    match filter {
        0 => out.copy_from_slice(cur_in),
        1 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                out[i] = cur_in[i].wrapping_add(left);
            }
        }
        2 => {
            for i in 0..n {
                out[i] = cur_in[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                let up = prev[i];
                out[i] = cur_in[i].wrapping_add(((left as u16 + up as u16) / 2) as u8);
            }
        }
        4 => {
            for i in 0..n {
                let left = if i >= bpp { out[i - bpp] } else { 0 };
                let up = prev[i];
                let upleft = if i >= bpp { prev[i - bpp] } else { 0 };
                out[i] = cur_in[i].wrapping_add(paeth(left, up, upleft));
            }
        }
        _ => out.copy_from_slice(cur_in),
    }
}

/// Encode an 8-bit RGBA image back to a PNG (filter type 0 = None per row).
pub fn encode_png(img: &PngImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(match img.channels {
        3 => 2,
        4 => 6,
        1 => 0,
        2 => 4,
        _ => return Err("unsupported channel count".into()),
    });
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Filtered data: each row prefixed with filter byte 0 (None).
    let stride = img.stride();
    let mut filtered = Vec::with_capacity((img.height as usize) * (stride + 1));
    for row in 0..img.height as usize {
        filtered.push(0);
        filtered.extend_from_slice(&img.data[row * stride..row * stride + stride]);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&filtered)
        .map_err(|e| format!("zlib deflate write: {e}"))?;
    let compressed = encoder
        .finish()
        .map_err(|e| format!("zlib deflate finish: {e}"))?;
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn write_chunk(out: &mut Vec<u8>, ctype: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ctype);
    out.extend_from_slice(data);
    let crc = crc32(ctype, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// CRC32 (PNG uses the IEEE polynomial).
fn crc32(ctype: &[u8], data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for n in 0..256u32 {
        let mut c = n;
        for _ in 0..8 {
            if c & 1 != 0 {
                c = 0xedb88320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
        }
        table[n as usize] = c;
    }
    let mut crc = 0xffffffff;
    for &b in ctype.iter().chain(data.iter()) {
        crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffffffff
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
fn draw_label(img: &mut PngImage, x: i64, y: i64, text: &str, color: [u8; 4]) {
    let mut cx = x;
    for ch in text.chars() {
        let upper = ch.to_ascii_uppercase().to_string();
        if let Some((_, glyph)) = FONT.iter().find(|(c, _)| *c == upper) {
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..5 {
                    if (bits >> (4 - col)) & 1 != 0 {
                        img.put_pixel(cx + col, y + row as i64, color);
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
        img.draw_rect(x, y, w, h, color);
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
                img.put_pixel(x + dx, y + dy, label_bg);
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

    fn solid_png(w: u32, h: u32, channels: u8, fill: u8) -> Vec<u8> {
        let img = PngImage {
            width: w,
            height: h,
            channels,
            data: vec![fill; (w as usize) * (h as usize) * channels as usize],
        };
        encode_png(&img).expect("encode")
    }

    #[test]
    fn png_round_trips_rgb() {
        let orig = solid_png(4, 3, 3, 7);
        let decoded = decode_png(&orig).expect("decode");
        assert_eq!(decoded.width, 4);
        assert_eq!(decoded.height, 3);
        assert_eq!(decoded.channels, 3);
        assert!(decoded.data.iter().all(|&b| b == 7));
    }

    #[test]
    fn png_round_trips_rgba() {
        let orig = solid_png(5, 5, 4, 9);
        let decoded = decode_png(&orig).expect("decode");
        assert_eq!(decoded.channels, 4);
        assert!(decoded.data.iter().all(|&b| b == 9));
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

    fn pixel(img: &PngImage, x: u32, y: u32) -> [u8; 4] {
        let stride = img.width as usize * img.channels as usize;
        let i = y as usize * stride + x as usize * img.channels as usize;
        match img.channels {
            3 => [img.data[i], img.data[i + 1], img.data[i + 2], 255],
            4 => [
                img.data[i],
                img.data[i + 1],
                img.data[i + 2],
                img.data[i + 3],
            ],
            _ => [0, 0, 0, 0],
        }
    }
}
