//! Screenshot handling for the smoke test: parse QEMU's PPM screendump and
//! write a PNG for humans. Dependency-free; the PNG uses uncompressed
//! ("stored") deflate blocks, which every decoder accepts.

use std::path::Path;

/// A packed 8-bit RGB image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RgbImage {
    /// The pixel at (`x`, `y`), or `None` outside the image.
    pub fn pixel(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let o = (y * self.width + x) * 3;
        Some([self.pixels[o], self.pixels[o + 1], self.pixels[o + 2]])
    }
}

/// Parse a binary PPM (`P6`, maxval 255).
pub fn parse_ppm(data: &[u8]) -> Result<RgbImage, String> {
    let mut pos = 0;
    let mut fields = [0usize; 3];
    let magic = next_token(data, &mut pos).ok_or("empty PPM")?;
    if magic != b"P6" {
        return Err("not a binary PPM (P6)".into());
    }
    for field in &mut fields {
        let token = next_token(data, &mut pos).ok_or("truncated PPM header")?;
        *field = std::str::from_utf8(token)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("bad number in PPM header")?;
    }
    let [width, height, maxval] = fields;
    if maxval != 255 {
        return Err(format!("unsupported PPM maxval {maxval}"));
    }
    // Exactly one whitespace byte separates the header from the pixels.
    pos += 1;
    let len = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .ok_or("PPM too large")?;
    let pixels = data
        .get(pos..pos + len)
        .ok_or("PPM pixel data truncated")?
        .to_vec();
    Ok(RgbImage {
        width,
        height,
        pixels,
    })
}

fn next_token<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    loop {
        while *pos < data.len() && data[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if data.get(*pos) == Some(&b'#') {
            while *pos < data.len() && data[*pos] != b'\n' {
                *pos += 1;
            }
            continue;
        }
        break;
    }
    let start = *pos;
    while *pos < data.len() && !data[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
    (start < *pos).then(|| &data[start..*pos])
}

/// Encode `image` as a PNG file.
pub fn encode_png(image: &RgbImage) -> Vec<u8> {
    let mut raw = Vec::with_capacity((image.width * 3 + 1) * image.height);
    for row in image.pixels.chunks_exact(image.width * 3) {
        raw.push(0); // filter type: none
        raw.extend_from_slice(row);
    }

    // zlib stream with stored deflate blocks.
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        zlib.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        let len = block.len() as u16;
        zlib.push(u8::from(last));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(image.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(image.height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour, no interlace

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"IDAT", &zlib);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

/// Write `image` to `path` as PNG.
pub fn save_png(image: &RgbImage, path: &Path) -> Result<(), String> {
    std::fs::write(path, encode_png(image))
        .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let table: [u32; 256] = std::array::from_fn(|n| {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        c
    });
    let mut crc = !0u32;
    for &byte in data {
        crc = table[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppm_with_comment() {
        let mut data = b"P6\n# made by a test\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let image = parse_ppm(&data).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixel(1, 0), Some([4, 5, 6]));
        assert_eq!(image.pixel(2, 0), None);
    }

    #[test]
    fn rejects_bad_ppm() {
        assert!(parse_ppm(b"").is_err());
        assert!(parse_ppm(b"P3\n1 1\n255\n").is_err());
        assert!(parse_ppm(b"P6\n2 2\n255\n\x00\x00").is_err());
        assert!(parse_ppm(b"P6\n1 1\n65535\n\x00\x00\x00\x00\x00\x00").is_err());
    }

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn png_has_valid_structure() {
        let image = RgbImage {
            width: 3,
            height: 2,
            pixels: vec![7; 18],
        };
        let png = encode_png(&image);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(png.ends_with(&[0xAE, 0x42, 0x60, 0x82]));
        assert_eq!(&png[12..16], b"IHDR");
    }
}
