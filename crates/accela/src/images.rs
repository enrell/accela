//! Inline images (kitty graphics / sixel) → GPU-ready textures, cached per image
//! generation so each image is decoded once.

use accela_vt::graphics::{Image, ImageFormat};
use gpui::RenderImage;
use image::{Frame, RgbaImage};
use std::collections::HashMap;
use std::sync::Arc;

/// Decode to BGRA (GPUI's texture format). `None` for corrupt data.
pub fn to_bgra(format: ImageFormat, width: u32, height: u32, data: &[u8]) -> Option<RgbaImage> {
    let mut img = match format {
        ImageFormat::Png => image::load_from_memory_with_format(data, image::ImageFormat::Png).ok()?.into_rgba8(),
        ImageFormat::Rgba => RgbaImage::from_raw(width, height, data.to_vec())?,
        ImageFormat::Rgb => {
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for px in data.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            RgbaImage::from_raw(width, height, rgba)?
        }
    };
    for px in img.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(img)
}

#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<u32, (u64, Option<Arc<RenderImage>>)>,
}

impl ImageCache {
    /// Texture for `img`, decoding on first use (and when the id is re-transmitted).
    pub fn get(&mut self, img: &Image) -> Option<Arc<RenderImage>> {
        if let Some((generation, tex)) = self.entries.get(&img.id)
            && *generation == img.generation
        {
            return tex.clone();
        }
        let tex = to_bgra(img.format, img.width, img.height, &img.data)
            .map(|buf| Arc::new(RenderImage::new(smallvec::smallvec![Frame::new(buf)])));
        self.entries.insert(img.id, (img.generation, tex.clone()));
        tex
    }

    /// Forget textures for images that are no longer referenced.
    pub fn retain(&mut self, live: impl Fn(u32) -> bool) {
        self.entries.retain(|id, _| live(*id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_and_rgba_become_bgra() {
        let px = to_bgra(ImageFormat::Rgb, 1, 1, &[1, 2, 3]).unwrap();
        assert_eq!(px.as_raw(), &[3, 2, 1, 255]);
        let px = to_bgra(ImageFormat::Rgba, 1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(px.as_raw(), &[3, 2, 1, 4]);
    }

    #[test]
    fn bad_data_is_rejected() {
        assert!(to_bgra(ImageFormat::Rgba, 2, 2, &[0; 3]).is_none());
        assert!(to_bgra(ImageFormat::Png, 1, 1, b"not a png").is_none());
    }

    #[test]
    fn png_roundtrip() {
        let mut png = Vec::new();
        let img = RgbaImage::from_raw(1, 1, vec![10, 20, 30, 255]).unwrap();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(to_bgra(ImageFormat::Png, 0, 0, &png).unwrap().as_raw(), &[30, 20, 10, 255]);
    }

    #[test]
    fn cache_decodes_once_per_generation() {
        let mut c = ImageCache::default();
        let mut img = Image {
            id: 1,
            width: 1,
            height: 1,
            format: ImageFormat::Rgb,
            data: Arc::new(vec![1, 2, 3]),
            generation: 1,
        };
        let a = c.get(&img).unwrap();
        assert!(Arc::ptr_eq(&a, &c.get(&img).unwrap()));
        img.generation = 2;
        assert!(!Arc::ptr_eq(&a, &c.get(&img).unwrap()));
        c.retain(|_| false);
        assert!(c.entries.is_empty());
    }
}
