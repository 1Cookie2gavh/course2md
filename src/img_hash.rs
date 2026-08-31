//! 感知哈希（dHash）与汉明距离：用于「新帧是否和上一张保留帧实质相同」判断。
//!
//! dHash：缩到 9×8 灰度，逐行比较相邻像素（左>右 置 1），得到 64-bit。

use crate::config::Roi;
use anyhow::Result;
use image::GenericImageView;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DHash(pub u64);

/// 计算图像 dHash；可选先裁剪 ROI（像素坐标按 `%`/像素约定换算）。
pub fn dhash(img: &image::DynamicImage, roi: Option<Roi>) -> DHash {
    let cropped: image::DynamicImage = match roi {
        None => img.clone(),
        Some(r) => {
            let (w, h) = img.dimensions();
            let (x, y, cw, ch) = {
                let (x1, y1, x2, y2) = r.pixels(w, h);
                (x1, y1, x2 - x1, y2 - y1)
            };
            img.crop_imm(x, y, cw, ch)
        }
    };
    let gray = cropped.resize_exact(9, 8, image::imageops::FilterType::Triangle).to_luma8();
    let px = |x: u32, y: u32| u64::from(gray.get_pixel(x, y)[0]);
    let mut hash: u64 = 0;
    let mut bit = 0;
    for y in 0..8 {
        for x in 0..8 {
            if px(x, y) > px(x + 1, y) {
                hash |= 1 << bit;
            }
            bit += 1;
        }
    }
    DHash(hash)
}

pub fn dhash_file(path: &Path, roi: Option<Roi>) -> Result<DHash> {
    let img = image::open(path)?;
    Ok(dhash(&img, roi))
}

pub fn hamming(a: DHash, b: DHash) -> u32 {
    (a.0 ^ b.0).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth(left_black: bool) -> image::DynamicImage {
        // 32×16 图：左半黑右半白（或反之）→ dHash 应显著不同
        let mut img = image::GrayImage::new(32, 16);
        for y in 0..16 {
            for x in 0..32 {
                let v = if (x < 16) == left_black { 0 } else { 255 };
                img.put_pixel(x, y, image::Luma([v]));
            }
        }
        image::DynamicImage::ImageLuma8(img)
    }

    #[test]
    fn identical_images_same_hash() {
        let a = synth(true);
        assert_eq!(dhash(&a, None), dhash(&a, None));
    }

    #[test]
    fn mirrored_images_far_apart() {
        let a = dhash(&synth(true), None);
        let b = dhash(&synth(false), None);
        // 垂直边缘镜像：只有边缘列的 bit 位置翻转，但足以区分（实测 ≥ 16）
        assert!(hamming(a, b) >= 12, "hamming={}", hamming(a, b));
    }

    #[test]
    fn uniform_vs_edge_far_apart() {
        let mut gray = image::GrayImage::new(32, 16);
        for p in gray.pixels_mut() {
            *p = image::Luma([128]);
        }
        let a = dhash(&image::DynamicImage::ImageLuma8(gray), None);
        let b = dhash(&synth(false), None); // 左白右黑（降序）→ 边缘列 bit 置 1
        assert!(hamming(a, b) >= 8, "hamming={}", hamming(a, b));
    }

    #[test]
    fn hamming_basics() {
        assert_eq!(hamming(DHash(0), DHash(0)), 0);
        assert_eq!(hamming(DHash(0b1010), DHash(0b1001)), 2);
        assert_eq!(hamming(DHash(!0u64), DHash(0)), 64);
    }
}
