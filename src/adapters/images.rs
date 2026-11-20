//! Images posted inside a Teams message as Graph hosted contents: no external host, no link.
//!
//! The `kuroko` process that receives the call reads a path (the host never opens one) and sends
//! the bytes to the host, which checks them again. The type comes only from the bytes (PNG,
//! JPEG, GIF), never from a name, an extension or a declared type. Graph caps a write request at
//! 4 MB and base64 adds a third, so all the images of a message share [`MAX_TOTAL`] bytes, which
//! leaves room for the band's animated mark (`card::MARK`).
//!
//! Graph refuses every GIF hosted content (HTTP 400, whatever its size), and Teams draws an
//! animated PNG, so a GIF becomes the animated PNG of its frames ([`gif_to_png`]) before it is
//! shown for confirmation: what the person approves is what is sent.
use anyhow::Result;
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Images in one message.
pub const MAX_IMAGES: usize = 4;
/// Bytes of all the images of one message together (about 3.7 MB once encoded).
pub const MAX_TOTAL: usize = 2_800_000;
/// Pixels of a GIF's canvas, and of all its frames together, that a conversion may decode: a
/// small file can describe a huge or endless animation.
const MAX_GIF_CANVAS: usize = 4_000_000;
const MAX_GIF_PIXELS: usize = 60_000_000;
/// Characters of an image's alternative text.
pub const MAX_ALT: usize = 200;
/// Characters of the name shown in the confirmation.
const MAX_NAME: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
}
impl Kind {
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
        }
    }
}

/// Why an image was not accepted: fixed explanations that are safe to show as they are.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageRefusal {
    TooMany,
    TooLarge,
    /// A GIF whose animated PNG would not fit, or that describes too much to convert.
    GifTooLarge,
    Unsupported,
    Unreadable,
    InvalidAlt,
    Invalid,
}
impl std::fmt::Display for ImageRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooMany => "A message carries at most 4 images.",
            Self::TooLarge => {
                "The images exceed 2.8 MB together, the most one Teams message can carry."
            }
            Self::GifTooLarge => {
                "Teams refuses GIF images, so a GIF is sent as an animated PNG, and this one would \
                 exceed 2.8 MB that way: use a smaller or shorter GIF."
            }
            Self::Unsupported => {
                "Only PNG, JPEG and GIF images can be posted (read from the file's bytes)."
            }
            Self::Unreadable => "The image could not be read: give the path of a regular file.",
            Self::InvalidAlt => {
                "An image's alt text must be at most 200 characters, without control characters."
            }
            Self::Invalid => "Each image needs exactly one of path or data (base64).",
        })
    }
}
impl std::error::Error for ImageRefusal {}

/// One checked image, with the bytes that are shown for confirmation and then sent.
#[derive(Clone, Debug)]
pub struct Image {
    pub name: String,
    pub alt: String,
    pub kind: Kind,
    /// The image arrived as a GIF and is its animated PNG.
    pub from_gif: bool,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}
impl Image {
    /// Checks `bytes` as an image: its type and size come from its own header.
    pub fn new(name: &str, alt: &str, bytes: Vec<u8>) -> Result<Self, ImageRefusal> {
        if bytes.len() > MAX_TOTAL {
            return Err(ImageRefusal::TooLarge);
        }
        let (mut kind, mut width, mut height) = detect(&bytes).ok_or(ImageRefusal::Unsupported)?;
        let from_gif = kind == Kind::Gif;
        let bytes = if from_gif {
            let png = gif_to_png(&bytes)?;
            (kind, width, height) = detect(&png).ok_or(ImageRefusal::Unsupported)?;
            png
        } else {
            bytes
        };
        let alt = alt.trim();
        if alt.chars().count() > MAX_ALT || alt.chars().any(char::is_control) {
            return Err(ImageRefusal::InvalidAlt);
        }
        let name: String = name
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_NAME)
            .collect();
        let name = if name.trim().is_empty() {
            format!("image.{}", kind.extension())
        } else if from_gif {
            // The name says what is sent: `homero.gif` goes as `homero.png`.
            let name = name.trim();
            let ends = |name: &str, suffix: &str| {
                name.len()
                    .checked_sub(suffix.len())
                    .filter(|at| name.is_char_boundary(*at))
                    .is_some_and(|at| name[at..].eq_ignore_ascii_case(suffix))
            };
            let stem = if ends(name, ".gif") {
                &name[..name.len() - 4]
            } else {
                name
            };
            if ends(stem, ".png") {
                stem.to_owned()
            } else {
                format!("{stem}.png")
            }
        } else {
            name.trim().to_owned()
        };
        Ok(Self {
            name,
            alt: alt.to_owned(),
            kind,
            from_gif,
            width,
            height,
            bytes,
        })
    }
    /// Reads a regular file, refusing one larger than an image may be before reading it.
    pub fn read(path: &Path, alt: &str) -> Result<Self, ImageRefusal> {
        let file = std::fs::File::open(path).map_err(|_| ImageRefusal::Unreadable)?;
        let metadata = file.metadata().map_err(|_| ImageRefusal::Unreadable)?;
        if !metadata.is_file() {
            return Err(ImageRefusal::Unreadable);
        }
        if metadata.len() > MAX_TOTAL as u64 {
            return Err(ImageRefusal::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take(MAX_TOTAL as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ImageRefusal::Unreadable)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self::new(&name, alt, bytes)
    }
    /// The first 8 hexadecimal digits of the SHA-256 of the bytes.
    pub fn digest(&self) -> String {
        crate::hash::hex(&Sha256::digest(&self.bytes)[..4])
    }
    /// What the person approves: `homero.png · PNG 480×400 · 1.05 MB · sha256 1a2b3c4d`, and
    /// `animated PNG from GIF` for a converted GIF.
    pub fn summary(&self) -> String {
        let mut line = format!(
            "{} · {} {}×{} · {} · sha256 {}",
            self.name,
            if self.from_gif {
                "animated PNG from GIF"
            } else {
                self.kind.label()
            },
            self.width,
            self.height,
            size(self.bytes.len()),
            self.digest()
        );
        if !self.alt.is_empty() {
            line.push_str(&format!(" · alt «{}»", self.alt));
        }
        line
    }
    /// The form the `kuroko` process sends to the host, which checks it again with [`from_wire`].
    pub fn wire(&self) -> Value {
        let mut wire = json!({"name": self.name, "alt": self.alt,
            "data": base64::engine::general_purpose::STANDARD.encode(&self.bytes)});
        if self.from_gif {
            wire["from_gif"] = json!(true);
        }
        wire
    }
    /// What a result and the confirmation report of an image: never its bytes.
    pub fn describe(&self) -> Value {
        let mut description = json!({"name": self.name, "type": self.kind.content_type(),
            "width": self.width, "height": self.height, "bytes": self.bytes.len(),
            "sha256": self.digest()});
        if self.from_gif {
            description["converted_from"] = json!(Kind::Gif.content_type());
        }
        description
    }
}

/// `1.05 MB`, `830.2 KB`, `512 B`.
pub fn size(bytes: usize) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        _ => format!("{:.2} MB", bytes as f64 / 1e6),
    }
}

/// An image as a tool argument: exactly one of `path` (read here) or `data` (base64), and an
/// optional `alt`. `name` names an image given as data.
pub fn from_argument(value: &Value) -> Result<Image, ImageRefusal> {
    let alt = match value.get("alt") {
        None | Some(Value::Null) => "",
        Some(Value::String(alt)) => alt,
        Some(_) => return Err(ImageRefusal::InvalidAlt),
    };
    match (value.get("path"), value.get("data")) {
        (Some(Value::String(path)), None) if !path.trim().is_empty() => {
            Image::read(Path::new(path.trim()), alt)
        }
        (None, Some(Value::String(_))) => from_wire(value),
        _ => Err(ImageRefusal::Invalid),
    }
}

/// An image as the host receives it: `{name, alt, data}`, with `data` in base64.
pub fn from_wire(value: &Value) -> Result<Image, ImageRefusal> {
    let data = value["data"].as_str().ok_or(ImageRefusal::Invalid)?;
    // A string longer than the largest encoding is refused before decoding it.
    if data.len() > MAX_TOTAL.div_ceil(3) * 4 + 4 {
        return Err(ImageRefusal::TooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| ImageRefusal::Unsupported)?;
    let alt = match &value["alt"] {
        Value::Null => "",
        Value::String(alt) => alt,
        _ => return Err(ImageRefusal::InvalidAlt),
    };
    let mut image = Image::new(value["name"].as_str().unwrap_or_default(), alt, bytes)?;
    // Converted where the file was read: the bytes are a PNG, and only the description says
    // where it came from.
    image.from_gif |= image.kind == Kind::Png && value["from_gif"] == json!(true);
    Ok(image)
}

/// The images of an argument list, checked one by one and together.
pub fn from_list(
    value: Option<&Value>,
    each: impl Fn(&Value) -> Result<Image, ImageRefusal>,
) -> Result<Vec<Image>, ImageRefusal> {
    let list = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(ImageRefusal::Invalid),
    };
    if list.len() > MAX_IMAGES {
        return Err(ImageRefusal::TooMany);
    }
    let images = list.iter().map(each).collect::<Result<Vec<_>, _>>()?;
    check(&images)?;
    Ok(images)
}

/// The limits of one message's images together.
pub fn check(images: &[Image]) -> Result<(), ImageRefusal> {
    if images.len() > MAX_IMAGES {
        return Err(ImageRefusal::TooMany);
    }
    if images.iter().map(|i| i.bytes.len()).sum::<usize>() > MAX_TOTAL {
        return Err(ImageRefusal::TooLarge);
    }
    Ok(())
}

/// The animated PNG of a GIF: each frame drawn over the previous ones and disposed of as GIF
/// says, with its delay (a delay of 0 or 1 hundredths plays at 10, as browsers do) and its
/// repetitions. Decoded twice, counting frames first, so only one canvas is ever held.
pub fn gif_to_png(bytes: &[u8]) -> Result<Vec<u8>, ImageRefusal> {
    let open = || {
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        options.set_memory_limit(gif::MemoryLimit::Bytes(
            std::num::NonZeroU64::new(4 * MAX_GIF_CANVAS as u64).expect("not zero"),
        ));
        options
            .read_info(bytes)
            .map_err(|_| ImageRefusal::Unsupported)
    };
    let mut decoder = open()?;
    let (width, height) = (usize::from(decoder.width()), usize::from(decoder.height()));
    let canvas_pixels = width * height;
    if canvas_pixels == 0 {
        return Err(ImageRefusal::Unsupported);
    }
    if canvas_pixels > MAX_GIF_CANVAS {
        return Err(ImageRefusal::GifTooLarge);
    }
    let mut frames = 0u32;
    while decoder
        .read_next_frame()
        .map_err(|_| ImageRefusal::Unsupported)?
        .is_some()
    {
        frames += 1;
        if frames as usize * canvas_pixels > MAX_GIF_PIXELS {
            return Err(ImageRefusal::GifTooLarge);
        }
    }
    if frames == 0 {
        return Err(ImageRefusal::Unsupported);
    }
    let plays = match decoder.repeat() {
        gif::Repeat::Infinite => 0,
        gif::Repeat::Finite(n) => u32::from(n) + 1,
    };
    let mut decoder = open()?;
    let mut out = Capped::default();
    // Writing stops at the first byte past what a message carries.
    let encoded = encode_frames(&mut decoder, &mut out, (width, height), frames, plays);
    if out.full {
        return Err(ImageRefusal::GifTooLarge);
    }
    encoded?;
    Ok(out.bytes)
}

/// The output of a conversion, refusing to grow past [`MAX_TOTAL`].
#[derive(Default)]
struct Capped {
    bytes: Vec<u8>,
    full: bool,
}
impl std::io::Write for Capped {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len() + data.len() > MAX_TOTAL {
            self.full = true;
            return Err(std::io::Error::other("larger than a message carries"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_frames(
    decoder: &mut gif::Decoder<&[u8]>,
    out: &mut Capped,
    (width, height): (usize, usize),
    frames: u32,
    plays: u32,
) -> Result<(), ImageRefusal> {
    let failed = |_| ImageRefusal::Unsupported;
    let mut encoder = png::Encoder::new(out, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    if frames > 1 {
        encoder.set_animated(frames, plays).map_err(failed)?;
    }
    let mut writer = encoder.write_header().map_err(failed)?;
    let mut canvas = vec![0u8; width * height * 4];
    while let Some(frame) = decoder
        .read_next_frame()
        .map_err(|_| ImageRefusal::Unsupported)?
    {
        let previous = (frame.dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
        let (left, top) = (usize::from(frame.left), usize::from(frame.top));
        let (frame_width, frame_height) = (usize::from(frame.width), usize::from(frame.height));
        let columns = frame_width.min(width.saturating_sub(left));
        for y in 0..frame_height.min(height.saturating_sub(top)) {
            for x in 0..columns {
                let from = (y * frame_width + x) * 4;
                let pixel = frame
                    .buffer
                    .get(from..from + 4)
                    .ok_or(ImageRefusal::Unsupported)?;
                // GIF transparency is all or nothing: a transparent pixel shows what is below.
                if pixel[3] != 0 {
                    let to = ((top + y) * width + left + x) * 4;
                    canvas[to..to + 4].copy_from_slice(pixel);
                }
            }
        }
        if frames > 1 {
            let delay = if frame.delay <= 1 { 10 } else { frame.delay };
            writer.set_frame_delay(delay, 100).map_err(failed)?;
        }
        writer.write_image_data(&canvas).map_err(failed)?;
        match (frame.dispose, previous) {
            (gif::DisposalMethod::Background, _) => {
                for y in top..(top + frame_height).min(height) {
                    let row = y * width;
                    canvas
                        [(row + left.min(width)) * 4..(row + (left + frame_width).min(width)) * 4]
                        .fill(0);
                }
            }
            (gif::DisposalMethod::Previous, Some(previous)) => canvas = previous,
            _ => {}
        }
    }
    writer.finish().map_err(failed)
}

/// The type and size of an image, read from its header; `None` for anything else.
pub fn detect(bytes: &[u8]) -> Option<(Kind, u32, u32)> {
    let be16 = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let (kind, width, height) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        if bytes.get(12..16)? != b"IHDR" {
            return None;
        }
        let be32 = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        (Kind::Png, be32(16)?, be32(20)?)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let le16 = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        (Kind::Gif, u32::from(le16(6)?), u32::from(le16(8)?))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // Walk the segments up to the frame header that holds the size.
        let mut at = 2;
        loop {
            if *bytes.get(at)? != 0xFF {
                return None;
            }
            let marker = *bytes.get(at + 1)?;
            match marker {
                0xFF => at += 1,
                0x01 | 0xD0..=0xD7 => at += 2,
                0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                    break (
                        Kind::Jpeg,
                        u32::from(be16(at + 7)?),
                        u32::from(be16(at + 5)?),
                    );
                }
                0xD9 | 0xDA => return None,
                _ => at += 2 + usize::from(be16(at + 2)?),
            }
        }
    } else {
        return None;
    };
    (width > 0 && height > 0).then_some((kind, width, height))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid header of each kind, `width`×`height`, padded to `len` bytes.
    pub(crate) fn png(width: u32, height: u32, len: usize) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.resize(len.max(bytes.len()), 0);
        bytes
    }
    /// A real two-frame GIF of `width`×`height` that loops forever, padded after its end to
    /// `len` bytes: a red frame for 20 hundredths, then one that paints the top-left quarter
    /// green, leaves the rest transparent (the red shows through) and is cleared afterwards.
    pub(crate) fn gif(width: u16, height: u16, len: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = gif::Encoder::new(&mut bytes, width, height, &[]).unwrap();
            encoder.set_repeat(gif::Repeat::Infinite).unwrap();
            let palette = [255, 0, 0, 0, 255, 0, 0, 0, 0];
            let pixels = |paint: &dyn Fn(u16, u16) -> u8| -> Vec<u8> {
                (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| paint(x, y))
                    .collect()
            };
            let mut red =
                gif::Frame::from_palette_pixels(width, height, pixels(&|_, _| 0), palette, None);
            red.delay = 20;
            encoder.write_frame(&red).unwrap();
            let quarter = |x: u16, y: u16| u8::from(!(x < width / 2 && y < height / 2)) + 1;
            let mut green =
                gif::Frame::from_palette_pixels(width, height, pixels(&quarter), palette, Some(2));
            green.dispose = gif::DisposalMethod::Background;
            encoder.write_frame(&green).unwrap();
        }
        bytes.resize(len.max(bytes.len()), 0);
        bytes
    }
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        // An APP0 segment before the frame header, as JFIF files have.
        bytes.extend([0xFF, 0xE0, 0x00, 0x10]);
        bytes.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        bytes.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        bytes.extend([0x03, 0x01, 0x22, 0x00]);
        bytes
    }

    #[test]
    fn the_type_and_size_come_from_the_bytes_only() {
        assert_eq!(detect(&png(640, 480, 64)), Some((Kind::Png, 640, 480)));
        assert_eq!(detect(&gif(480, 400, 64)), Some((Kind::Gif, 480, 400)));
        assert_eq!(detect(&jpeg(1024, 768)), Some((Kind::Jpeg, 1024, 768)));
        // WebP, SVG, text and truncated or empty headers are not images here.
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        webp.resize(64, 0);
        for bytes in [
            webp,
            b"<svg xmlns='http://www.w3.org/2000/svg'/>".to_vec(),
            b"GIF89a".to_vec(),
            png(0, 10, 64),
            vec![0xFF, 0xD8, 0xFF, 0xD9],
            Vec::new(),
        ] {
            assert_eq!(detect(&bytes), None, "{bytes:?}");
        }
        // A file named .png holding a GIF is a GIF, sent as its animated PNG.
        let image = Image::new("photo.png", "", gif(2, 2, 32)).unwrap();
        assert!(image.from_gif);
        assert_eq!((image.kind, image.name.as_str()), (Kind::Png, "photo.png"));
    }

    #[test]
    fn data_in_any_other_shape_is_refused() {
        let data = base64::engine::general_purpose::STANDARD.encode(gif(2, 2, 32));
        let both = json!({"path": "/tmp/x.gif", "data": data});
        assert_eq!(from_argument(&both).unwrap_err(), ImageRefusal::Invalid);
        assert_eq!(
            from_argument(&json!({})).unwrap_err(),
            ImageRefusal::Invalid
        );
        assert_eq!(
            from_argument(&json!({"data": "%%%"})).unwrap_err(),
            ImageRefusal::Unsupported
        );
        let text = base64::engine::general_purpose::STANDARD.encode("hola");
        assert_eq!(
            from_argument(&json!({"data": text})).unwrap_err(),
            ImageRefusal::Unsupported
        );
        let named = from_argument(&json!({"data": data})).unwrap();
        assert_eq!(named.name, "image.png");
    }

    #[test]
    fn count_size_and_alt_are_limited_in_code() {
        let one = Image::new("a.gif", "", gif(2, 2, 32)).unwrap();
        assert!(check(&vec![one.clone(); MAX_IMAGES]).is_ok());
        assert_eq!(
            check(&vec![one; MAX_IMAGES + 1]).unwrap_err(),
            ImageRefusal::TooMany
        );
        // One image over the limit, and two that pass alone but not together.
        assert_eq!(
            Image::new("big.png", "", png(9, 9, MAX_TOTAL + 1)).unwrap_err(),
            ImageRefusal::TooLarge
        );
        let half = Image::new("half.png", "", png(9, 9, MAX_TOTAL / 2 + 1)).unwrap();
        assert_eq!(
            check(&[half.clone(), half]).unwrap_err(),
            ImageRefusal::TooLarge
        );
        let long = "a".repeat(MAX_ALT + 1);
        assert_eq!(
            Image::new("a.gif", &long, gif(2, 2, 32)).unwrap_err(),
            ImageRefusal::InvalidAlt
        );
        assert_eq!(
            Image::new("a.gif", "line\nbreak", gif(2, 2, 32)).unwrap_err(),
            ImageRefusal::InvalidAlt
        );
        let list: Vec<Value> = (0..=MAX_IMAGES).map(|_| json!({"data": ""})).collect();
        assert_eq!(
            from_list(Some(&json!(list)), from_wire).unwrap_err(),
            ImageRefusal::TooMany
        );
    }

    #[test]
    fn a_path_is_read_only_when_it_is_a_small_enough_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("homero.gif");
        std::fs::write(&file, gif(480, 400, 1_050_994)).unwrap();
        let image = Image::read(&file, "Homero").unwrap();
        assert_eq!(
            image.summary(),
            format!(
                "homero.png · animated PNG from GIF 480×400 · {} · sha256 {} · alt «Homero»",
                size(image.bytes.len()),
                image.digest()
            )
        );
        assert_eq!(image.digest().len(), 8);
        // The wire form carries the same bytes back, still described as a converted GIF.
        let back = from_wire(&image.wire()).unwrap();
        assert_eq!(back.bytes, image.bytes);
        assert_eq!(back.summary(), image.summary());
        assert_eq!(
            (back.name.as_str(), back.alt.as_str()),
            ("homero.png", "Homero")
        );
        assert_eq!(
            Image::read(dir.path(), "").unwrap_err(),
            ImageRefusal::Unreadable
        );
        assert_eq!(
            Image::read(&dir.path().join("missing.gif"), "").unwrap_err(),
            ImageRefusal::Unreadable
        );
        let big = dir.path().join("big.png");
        std::fs::write(&big, png(9, 9, MAX_TOTAL + 1)).unwrap();
        assert_eq!(Image::read(&big, "").unwrap_err(), ImageRefusal::TooLarge);
    }

    #[test]
    fn a_gif_becomes_the_animated_png_of_its_frames() {
        let image = Image::new("loop.GIF", "", gif(8, 6, 0)).unwrap();
        assert_eq!(image.name, "loop.png");
        assert_eq!((image.kind, image.width, image.height), (Kind::Png, 8, 6));
        assert_eq!(image.describe()["converted_from"], "image/gif");
        assert_eq!(image.describe()["type"], "image/png");
        let mut reader = png::Decoder::new(std::io::Cursor::new(&image.bytes))
            .read_info()
            .unwrap();
        let animation = reader.info().animation_control.unwrap();
        // Two frames, played forever.
        assert_eq!((animation.num_frames, animation.num_plays), (2, 0));
        let mut frame = vec![0; reader.output_buffer_size().unwrap()];
        let mut frames = Vec::new();
        while let Ok(output) = reader.next_frame(&mut frame) {
            let control = reader.info().frame_control.unwrap();
            frames.push((
                control.delay_num,
                control.delay_den,
                frame[..output.buffer_size()].to_vec(),
            ));
        }
        assert_eq!(frames.len(), 2);
        let pixel = |frame: &[u8], x: usize, y: usize| frame[(y * 8 + x) * 4..][..4].to_vec();
        // The first plays its 20 hundredths; the second, with no delay, plays at 10.
        assert_eq!((frames[0].0, frames[0].1), (20, 100));
        assert_eq!((frames[1].0, frames[1].1), (10, 100));
        assert_eq!(pixel(&frames[0].2, 7, 5), [255, 0, 0, 255]);
        // The second frame is drawn over the first: green where it paints, red where it is
        // transparent.
        assert_eq!(pixel(&frames[1].2, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&frames[1].2, 7, 5), [255, 0, 0, 255]);
    }

    #[test]
    fn a_gif_that_cannot_be_converted_is_refused() {
        // A header with no image after it.
        assert_eq!(
            Image::new("a.gif", "", b"GIF89a\x02\x00\x02\x00".to_vec()).unwrap_err(),
            ImageRefusal::Unsupported
        );
        // A canvas larger than a conversion may decode, however small the file.
        assert_eq!(
            gif_to_png(&gif(2_100, 2_000, 0)).unwrap_err(),
            ImageRefusal::GifTooLarge
        );
        assert!(
            ImageRefusal::GifTooLarge
                .to_string()
                .contains("animated PNG")
        );
    }
}

#[cfg(test)]
mod hash_pins {
    use super::*;

    #[test]
    fn the_image_digest_is_the_first_eight_digits_of_the_sha256() {
        let header = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x02\0\0\0\x02";
        let image = Image::new("a.png", "alt", header.to_vec()).unwrap();
        assert_eq!(image.digest(), "a14958fa");
    }
}
