//! Downsample compact raster artwork at the current display's physical pixel size.

use gpui::{IntoElement, Styled, StyledImage, px};
use image::GenericImageView as _;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// Keep source artwork intact; cache display-sized images as the window moves between monitors.
pub struct Artwork {
    source: image::RgbaImage,
    inset: f32,
    radius: f32,
    sizes: Mutex<HashMap<u32, Arc<gpui::Image>>>,
}

impl Artwork {
    pub fn png(bytes: &[u8], inset: f32, radius: f32) -> Self {
        Self { source: decode(bytes), inset, radius, sizes: Mutex::new(HashMap::new()) }
    }

    pub fn element(&'static self, size: f32) -> impl IntoElement {
        ArtworkElement { artwork: self, size }
    }

    fn image(&self, side: u32) -> Arc<gpui::Image> {
        self.sizes
            .lock()
            .expect("artwork cache")
            .entry(side)
            .or_insert_with(|| {
                let pixels = resample(&self.source, side, self.inset, self.radius);
                let mut png = std::io::Cursor::new(Vec::new());
                pixels.write_to(&mut png, image::ImageFormat::Png).expect("encode artwork");
                Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png.into_inner()))
            })
            .clone()
    }
}

#[derive(gpui::IntoElement)]
struct ArtworkElement {
    artwork: &'static Artwork,
    size: f32,
}

impl gpui::RenderOnce for ArtworkElement {
    fn render(self, window: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        let side = (self.size * window.scale_factor()).ceil().max(1.) as u32;
        gpui::img(self.artwork.image(side)).size(px(self.size)).object_fit(gpui::ObjectFit::Contain).flex_shrink_0()
    }
}

fn decode(source: &[u8]) -> image::RgbaImage {
    image::load_from_memory_with_format(source, image::ImageFormat::Png).expect("bundled artwork").to_rgba8()
}

fn resample(source: &image::RgbaImage, side: u32, inset: f32, radius: f32) -> image::RgbaImage {
    let trim = (source.width().min(source.height()) as f32 * inset).round() as u32;
    let cropped = image::imageops::crop_imm(source, trim, trim, source.width() - 2 * trim, source.height() - 2 * trim);
    // Filter premultiplied color so hidden RGB in transparent pixels cannot create a fringe.
    let premultiplied = image::Rgba32FImage::from_fn(cropped.width(), cropped.height(), |x, y| {
        let p = cropped.get_pixel(x, y);
        // image::resize clamps float channels to 1. Reserve headroom so Lanczos's
        // overshoot doesn't clip alpha independently of color and create a bright rim.
        let alpha = f32::from(p[3]) / 255. * 0.5;
        image::Rgba([f32::from(p[0]) / 255. * alpha, f32::from(p[1]) / 255. * alpha, f32::from(p[2]) / 255. * alpha, alpha])
    });
    let scaled = image::imageops::resize(&premultiplied, side, side, image::imageops::FilterType::Lanczos3);
    let r = side as f32 * radius;
    image::RgbaImage::from_fn(side, side, |x, y| {
        let p = scaled.get_pixel(x, y);
        let alpha = (p[3] * 2.).clamp(0., 1.);
        let mut covered = 0;
        // Supersample only the corner mask; do not soften the artwork's interior details.
        for sy in 0..8 {
            for sx in 0..8 {
                let dx = ((x as f32 + (sx as f32 + 0.5) / 8.) - side as f32 / 2.).abs() - (side as f32 / 2. - r);
                let dy = ((y as f32 + (sy as f32 + 0.5) / 8.) - side as f32 / 2.).abs() - (side as f32 / 2. - r);
                if dx.max(0.).powi(2) + dy.max(0.).powi(2) <= r * r {
                    covered += 1;
                }
            }
        }
        let coverage = covered as f32 / 64.;
        if alpha <= 0.00001 {
            return image::Rgba([0, 0, 0, 0]);
        }
        // Retain valid RGB beneath the transparent mask too, so the GPU's final
        // fractional-pixel interpolation cannot blend a black matte back into the rim.
        // Unpremultiply before clamping coverage; the reserved headroom cancels here.
        let channel = |c: f32| ((c / p[3]).clamp(0., 1.) * 255.).round() as u8;
        image::Rgba([channel(p[0]), channel(p[1]), channel(p[2]), (alpha * coverage * 255.).round() as u8])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rasterize(source: &[u8], side: u32, inset: f32, radius: f32) -> image::RgbaImage {
        resample(&decode(source), side, inset, radius)
    }

    #[test]
    fn trimming_a_source_rim_keeps_a_light_top_edge() {
        let mut source = image::RgbaImage::from_pixel(128, 128, image::Rgba([0, 0, 0, 255]));
        for y in 8..120 {
            for x in 8..120 {
                source.put_pixel(x, y, image::Rgba([248, 236, 220, 255]));
            }
        }
        for scale in [1., 1.25, 1.5, 2.] {
            let side = (46_f32 * scale).ceil() as u32;
            let image = resample(&source, side, 0.0625, 0.22);
            let top = image.get_pixel(side / 2, 0);
            assert!(top[0] > 150 && top[1] > 150 && top[2] > 150, "dark source matte survives at {scale}x: {top:?}");
        }
    }

    #[test]
    fn rounded_edges_have_partial_coverage_without_a_color_halo() {
        // Opaque cream with hidden black pixels outside its hard-cut corners.
        let mut source = image::RgbaImage::from_pixel(64, 64, image::Rgba([248, 236, 220, 255]));
        for y in 0..64 {
            for x in 0..64 {
                if x < 10 && y < 10 {
                    source.put_pixel(x, y, image::Rgba([0, 0, 0, 0]));
                }
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let result = rasterize(png.get_ref(), 23, 0., 0.22);
        let edge: Vec<_> = result.pixels().filter(|p| p[3] > 0 && p[3] < 255).collect();
        assert!(edge.len() > 12, "rounding needs coverage across the curved perimeter");
        assert!(edge.iter().all(|p| p[0] > 235 && p[1] > 225 && p[2] > 210), "hidden black leaked into the edge");
        for p in result.pixels().filter(|p| p[3] > 0) {
            for (channel, expected) in p.0[..3].iter().zip([248_u8, 236, 220]) {
                assert!(channel.abs_diff(expected) <= 1, "filtering introduced a dark or bright rim: {p:?}");
            }
        }
    }
}
