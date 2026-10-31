//! Pictures attached to a message: read from the clipboard or a dropped file, shrunk to what
//! an agent will take, and sent to it as ACP image blocks (or, for dex, as a file it can open).
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, atomic::{AtomicU64, Ordering}},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use gpui::ImageFormat;
use image::{DynamicImage, ImageFormat as Codec, ImageReader, imageops::FilterType};
use serde_json::{Value, json};

/// The most an agent is sent of one picture; APIs refuse around 5 MB of base64, which is a
/// little under 3.75 MB of bytes.
const MAX_BYTES: usize = 3_500_000;
/// The longest side kept as it is. Models resize anything larger themselves, so more pixels
/// only cost upload time.
const MAX_SIDE: u32 = 2000;
/// What a picture over either limit is shrunk to fit.
const TARGET_SIDE: u32 = 1568;
/// A picture is never shrunk below this to meet the byte limit.
const MIN_SIDE: u32 = 256;
/// Files bigger than this are not even read.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// How many pictures one message holds.
pub const MAX_PER_MESSAGE: usize = 8;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A picture ready to send, and to show.
pub struct Attachment {
    pub image: Arc<gpui::Image>,
    /// Its width and height in pixels.
    pub size: (u32, u32),
    /// Where dex is told to find it, once saved.
    saved: OnceLock<PathBuf>,
}

impl Attachment {
    fn new(format: ImageFormat, bytes: Vec<u8>) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let size = ImageReader::new(Cursor::new(&bytes)).with_guessed_format().ok().and_then(|reader| reader.into_dimensions().ok()).unwrap_or((MAX_SIDE, MAX_SIDE));
        Self { image: Arc::new(gpui::Image { format, bytes, id }), size, saved: OnceLock::new() }
    }

    /// Takes the bytes of an image file or clipboard image, shrinking it if an agent would refuse it.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let codec = image::guess_format(&bytes).context("not an image")?;
        let format = format_of(codec).context("that image format can't be sent")?;
        let (width, height) = ImageReader::with_format(Cursor::new(&bytes), codec).into_dimensions()?;
        if bytes.len() <= MAX_BYTES && width.max(height) <= MAX_SIDE {
            return Ok(Self::new(format, bytes));
        }
        let decoded = image::load_from_memory_with_format(&bytes, codec)?;
        let (format, bytes) = shrink(decoded)?;
        Ok(Self::new(format, bytes))
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        if !has_image_extension(path) {
            bail!("{} is not an image", path.display());
        }
        if std::fs::metadata(path)?.len() > MAX_FILE {
            bail!("{} is too big", path.display());
        }
        Self::from_bytes(std::fs::read(path)?)
    }

    /// An ACP image block, or `None` for one that isn't a usable picture.
    pub fn from_block(block: &Value) -> Option<Self> {
        let data = block["data"].as_str()?;
        Self::from_bytes(STANDARD.decode(data).ok()?).ok()
    }

    pub fn block(&self) -> Value {
        json!({ "type": "image", "mimeType": self.image.format.mime_type(), "data": STANDARD.encode(&self.image.bytes) })
    }

    /// The picture as a `data:` URI, so a screen drawn without file access can show it.
    pub fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.image.format.mime_type(), STANDARD.encode(&self.image.bytes))
    }

    /// Writes the picture where an agent that can't be sent one can open it.
    pub fn saved(&self) -> Option<&Path> {
        if self.saved.get().is_none() {
            let dir = crate::config::data_dir().join("attachments");
            let path = dir.join(format!("{}-{}.{}", std::process::id(), self.image.id, self.image.format.extension()));
            let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &self.image.bytes));
            match written {
                Ok(()) => drop(self.saved.set(path)),
                Err(error) => log::warn!("attachment: {}: {error}", path.display()),
            }
        }
        self.saved.get().map(PathBuf::as_path)
    }
}

/// What dex is told in place of the pictures it can't be sent.
pub fn note(attachments: &[Arc<Attachment>]) -> String {
    let paths: Vec<String> = attachments.iter().filter_map(|a| a.saved()).map(|p| format!("`{}`", p.display())).collect();
    match paths.len() {
        0 => String::new(),
        1 => format!("\n\n[The user attached an image, saved at {}.]", paths[0]),
        _ => format!("\n\n[The user attached images, saved at {}.]", paths.join(", ")),
    }
}

fn has_image_extension(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif")
}

fn format_of(codec: Codec) -> Option<ImageFormat> {
    match codec {
        Codec::Png => Some(ImageFormat::Png),
        Codec::Jpeg => Some(ImageFormat::Jpeg),
        Codec::WebP => Some(ImageFormat::Webp),
        Codec::Gif => Some(ImageFormat::Gif),
        _ => None,
    }
}

/// Scales `picture` down until it is within the limits: JPEG, unless it has transparency.
fn shrink(picture: DynamicImage) -> Result<(ImageFormat, Vec<u8>)> {
    let opaque = !picture.color().has_alpha();
    let mut side = picture.width().max(picture.height()).min(TARGET_SIDE);
    loop {
        let scaled = if picture.width().max(picture.height()) > side { picture.resize(side, side, FilterType::Triangle) } else { picture.clone() };
        let mut out = Cursor::new(Vec::new());
        if opaque {
            scaled.to_rgb8().write_to(&mut out, Codec::Jpeg)?;
        } else {
            scaled.write_to(&mut out, Codec::Png)?;
        }
        let bytes = out.into_inner();
        if bytes.len() <= MAX_BYTES || side <= MIN_SIDE {
            return Ok((if opaque { ImageFormat::Jpeg } else { ImageFormat::Png }, bytes));
        }
        side = (side * 3 / 4).max(MIN_SIDE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(width, height).write_to(&mut out, Codec::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_small_picture_is_sent_as_it_is() {
        let bytes = png(40, 30);
        let attachment = Attachment::from_bytes(bytes.clone()).unwrap();
        assert_eq!(attachment.image.bytes, bytes);
        assert_eq!(attachment.block()["mimeType"], "image/png");
    }

    #[test]
    fn a_huge_picture_is_scaled_down() {
        let attachment = Attachment::from_bytes(png(4000, 1000)).unwrap();
        let shrunk = image::load_from_memory(&attachment.image.bytes).unwrap();
        assert_eq!((shrunk.width(), shrunk.height()), (TARGET_SIDE, TARGET_SIDE / 4));
    }

    #[test]
    fn what_is_not_an_image_is_refused() {
        assert!(Attachment::from_bytes(b"hello".to_vec()).is_err());
    }

    #[test]
    fn a_block_comes_back_as_the_same_picture() {
        let attachment = Attachment::from_bytes(png(8, 8)).unwrap();
        let again = Attachment::from_block(&attachment.block()).unwrap();
        assert_eq!(again.image.bytes, attachment.image.bytes);
    }
}
