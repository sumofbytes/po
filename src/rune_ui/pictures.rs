//! The model's pictures drawn on Rune: the `data:` URIs of attachments, decoded by an image cache
//! of one. Screens build `el` during the frame, so the cache is a global set once at setup; entries
//! are shared by content, so re-asking for the same picture reuses its pixels.
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use rune::image::ImageCache;
use rune::{El, SurfaceFit};

/// A thumbnail in the transcript's user row, and the picture shown large over the window.
pub const THUMB: (f32, f32) = (56.0, 42.0);
pub const PREVIEW: (f32, f32) = (900.0, 640.0);
/// How large a picture draws in the strip under the field.
pub const SMALL: (f32, f32) = (110.0, 82.0);

static IMAGES: OnceLock<Mutex<Option<ImageCache>>> = OnceLock::new();

/// Takes the setup waker, so a decode finishing wakes the frame it belongs to.
pub fn set(waker: rune::Waker) {
    *IMAGES
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(ImageCache::new(waker));
}

/// The box the picture fills, as large as fits `large` without a larger side than it has.
fn fit(size: (u32, u32), large: (f32, f32)) -> (f32, f32) {
    let (pw, ph) = (size.0.max(1) as f32, size.1.max(1) as f32);
    let scale = (large.0 / pw).min(large.1 / ph).min(1.0);
    ((pw * scale).round(), (ph * scale).round())
}

/// The picture as an element that fills at most `large`, keeping its aspect, or an empty one when
/// the URI is not a `data:` URI that decodes.
pub fn el(uri: &str, size: (u32, u32), large: (f32, f32)) -> El {
    let (w, h) = fit(size, large);
    let mut e = rune::el()
        .key(format!("picture-{}-{}", w as u32, h as u32))
        .size(w, h);
    let Some((mime, data)) = uri
        .strip_prefix("data:")
        .and_then(|rest| rest.rsplit_once(";base64,"))
    else {
        return e;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return e;
    };
    if let Some(images) = IMAGES.get() {
        let cache = images.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cache) = cache.as_ref() {
            // An SVG needs `scale` to rasterize at; the rest decode to their own size.
            let image = cache.load_mime(bytes.as_slice(), mime, large.0, 2.0);
            e = e.pixels_fit(image.pixels(), SurfaceFit::Fit);
        }
    }
    e
}
