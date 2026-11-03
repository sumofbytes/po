// Modified by Longbridge for gpui-fast.
//! Renderers for the extra GPUI surfaces of a composed window.
//!
//! Ported from zed-industries/zed#62379. A window that composes native views
//! between GPUI's base content and its overlays draws each GPUI surface on a
//! `CAMetalLayer` of its own. Every surface's renderer shares the window
//! renderer's device and sprite atlas, so glyphs and images rasterized for
//! one surface are there for the others, and the tiles a frame's scene refers
//! to are valid whichever surface draws them.

use std::sync::Arc;

    let layer = CAMetalLayer::new();
    MetalRenderer::configure_layer(&layer, &device, transparent, false);
    copy_colorspace(base, &layer);
    let mut renderer =
        MetalRenderer::new_internal(device, Some(layer), !transparent, instance_buffer_pool)?;
    renderer.sprite_atlas = base.sprite_atlas().clone();
    Ok(renderer)
}

/// Gives `layer` the colorspace of `base`'s layer, so that overlays above a
/// window drawn in Display P3 are drawn in it too.
fn copy_colorspace(base: &MetalRenderer, layer: &CAMetalLayer) {
    let Some(base_layer) = base.layer.as_ref() else {
        return;
    };
    layer.setColorspace(base_layer.colorspace().as_deref());
}
