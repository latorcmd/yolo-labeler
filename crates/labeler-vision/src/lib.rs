//! Image detection and quality metrics shared by the desktop frontends.
//!
//! This crate deliberately has no dependency on the project model or a GUI.
//! Coordinates use the source image's unrotated pixel space, matching YOLO
//! labels in the project.

pub mod augment;

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use image::{DynamicImage, GrayImage, Luma, RgbImage};
use imageproc::{
    contrast::{ThresholdType, otsu_level, threshold},
    region_labelling::{Connectivity, connected_components},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Increment when quality metrics or their interpretation changes.
pub const ALGORITHM_VERSION: &str = "labeler-vision-rust-1";
/// Explicit alias for callers that expose multiple algorithm versions.
pub const QUALITY_ALGORITHM_VERSION: &str = ALGORITHM_VERSION;

const GRAY_R: u64 = 4_899;
const GRAY_G: u64 = 9_617;
const GRAY_B: u64 = 1_868;
const GRAY_SHIFT: u32 = 14;
const GRAY_ROUND: u64 = 1 << (GRAY_SHIFT - 1);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedBox {
    pub class_id: usize,
    pub cx: f64,
    pub cy: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    pub threshold: Option<u8>,
    pub min_area_ratio: f64,
    pub merge_gap_ratio: f64,
    pub merge_component_ratio: f64,
    pub padding: u32,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            threshold: None,
            min_area_ratio: 0.001,
            merge_gap_ratio: 0.20,
            merge_component_ratio: 0.05,
            padding: 0,
        }
    }
}

impl DetectorConfig {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("min_area_ratio", self.min_area_ratio),
            ("merge_gap_ratio", self.merge_gap_ratio),
            ("merge_component_ratio", self.merge_component_ratio),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                bail!("{name} must be a finite number between 0 and 1");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub boxes: Vec<NormalizedBox>,
    pub method: String,
    pub threshold: u8,
    pub component_count: usize,
    pub flags: Vec<String>,
    pub foreground_ratio: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quality {
    pub sha256: String,
    pub sharpness: f64,
    pub brightness: f64,
    pub dark_fraction: f64,
    pub bright_fraction: f64,
    pub min_target_pixels: u32,
    pub dhash: String,
    pub flags: Vec<String>,
    pub score: f64,
    pub algorithm_version: String,
    pub parameter_fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelBox {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl PixelBox {
    fn right(self) -> u32 {
        self.x + self.width
    }

    fn bottom(self) -> u32 {
        self.y + self.height
    }

    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

#[derive(Clone, Copy, Debug)]
struct Component {
    bounds: PixelBox,
    area: u64,
    first_pixel: u64,
}

/// Detect one primary object and merge nearby connected components into it.
///
/// The detector returns class ID 0 as a placeholder; class assignment belongs
/// to the project/core layer. EXIF orientation is never applied here.
pub fn detect(image: &DynamicImage, config: &DetectorConfig) -> Result<Detection> {
    detect_internal(image, config, false)
}

/// Detect with Otsu's threshold even if a manual threshold was configured.
/// This mode uses the Otsu mask directly, without the alternate HSV threshold.
pub fn detect_otsu(image: &DynamicImage, config: &DetectorConfig) -> Result<Detection> {
    detect_internal(image, config, true)
}

fn detect_internal(
    image: &DynamicImage,
    config: &DetectorConfig,
    otsu_only: bool,
) -> Result<Detection> {
    config.validate()?;
    let width = image.width();
    let height = image.height();
    ensure_dimensions(width, height)?;

    let rgb = image.to_rgb8();
    let gray = rgb_to_gray(&rgb);
    let pixel_count = u64::from(width) * u64::from(height);
    let mut flags = Vec::new();

    let manual_threshold = if otsu_only { None } else { config.threshold };
    let (mut mask, mut method, mut chosen_threshold) = match manual_threshold {
        Some(value) => (
            threshold(&gray, value, ThresholdType::BinaryInverted),
            "manual".to_owned(),
            value,
        ),
        None => {
            let value = otsu_level(&gray);
            (
                threshold(&gray, value, ThresholdType::BinaryInverted),
                "otsu".to_owned(),
                value,
            )
        }
    };

    let initial_foreground_ratio = foreground_ratio(&mask, pixel_count);
    let (mut bounds, mut component_count) = if otsu_only && initial_foreground_ratio == 1.0 {
        (None, 0)
    } else {
        select_components(&mask, config)?
    };

    if !otsu_only
        && manual_threshold.is_none()
        && (initial_foreground_ratio > 0.5
            || bounds.is_some_and(|bounds| bounds.area() as f64 / pixel_count as f64 > 0.9))
    {
        let value_mask = GrayImage::from_fn(width, height, |x, y| {
            let value = rgb.get_pixel(x, y).0.into_iter().max().unwrap_or(0);
            Luma([if value < 60 { 255 } else { 0 }])
        });
        mask = value_mask;
        (bounds, component_count) = select_components(&mask, config)?;
        chosen_threshold = 60;
        method = "hsv_v60".to_owned();
        flags.push("fallback_used".to_owned());
    }

    let final_foreground_ratio = foreground_ratio(&mask, pixel_count);
    if final_foreground_ratio > 0.5 {
        flags.push("large_foreground".to_owned());
    }

    let boxes = if let Some(bounds) = bounds {
        if bounds.area() as f64 / pixel_count as f64 > 0.9 {
            flags.push("large_box".to_owned());
        }
        if component_count > 1 {
            flags.push("merged_components".to_owned());
        }

        let x = bounds.x.saturating_sub(config.padding);
        let y = bounds.y.saturating_sub(config.padding);
        let right = bounds.right().saturating_add(config.padding).min(width);
        let bottom = bounds.bottom().saturating_add(config.padding).min(height);
        let padded = PixelBox {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        };
        if padded.x == 0 || padded.y == 0 || padded.right() == width || padded.bottom() == height {
            flags.push("touches_border".to_owned());
        }
        vec![NormalizedBox {
            class_id: 0,
            cx: (f64::from(padded.x) + f64::from(padded.width) / 2.0) / f64::from(width),
            cy: (f64::from(padded.y) + f64::from(padded.height) / 2.0) / f64::from(height),
            width: f64::from(padded.width) / f64::from(width),
            height: f64::from(padded.height) / f64::from(height),
        }]
    } else {
        Vec::new()
    };

    Ok(Detection {
        boxes,
        method,
        threshold: chosen_threshold,
        component_count,
        flags,
        foreground_ratio: final_foreground_ratio,
    })
}

/// Compute blur, lighting, target-size, and difference-hash metrics.
///
/// The source bytes are read once so the SHA-256 and decoded pixels describe
/// the same file contents. EXIF orientation is not applied.
pub fn quality(path: &Path, boxes: &[NormalizedBox], blur_threshold: f64) -> Result<Quality> {
    if !blur_threshold.is_finite() {
        bail!("blur_threshold must be finite");
    }
    let payload =
        fs::read(path).with_context(|| format!("cannot read image: {}", path.display()))?;
    let image = image::load_from_memory(&payload)
        .with_context(|| format!("cannot decode image: {}", path.display()))?;
    let rgb = image.to_rgb8();
    let width = rgb.width();
    let height = rgb.height();
    ensure_dimensions(width, height)?;
    let gray = rgb_to_gray(&rgb);

    let mut roi_scores = Vec::new();
    let mut minimum_side = width.min(height);
    for bbox in boxes {
        validate_box(bbox)?;
        let x1 = normalized_floor(bbox.cx - bbox.width / 2.0, width).min(width);
        let y1 = normalized_floor(bbox.cy - bbox.height / 2.0, height).min(height);
        let x2 = normalized_ceil(bbox.cx + bbox.width / 2.0, width).min(width);
        let y2 = normalized_ceil(bbox.cy + bbox.height / 2.0, height).min(height);
        if x2 <= x1 || y2 <= y1 {
            continue;
        }
        let roi_width = x2 - x1;
        let roi_height = y2 - y1;
        if roi_width > 1 && roi_height > 1 {
            minimum_side = minimum_side.min(roi_width.min(roi_height));
            let roi = crop_gray(&gray, x1, y1, roi_width, roi_height);
            let resized = resize_area(&roi, 128, 128);
            roi_scores.push(laplacian_variance(&resized));
        }
    }

    if roi_scores.is_empty() {
        let normalized = resize_area(&gray, 256, 256);
        roi_scores.push(laplacian_variance(&normalized));
    }
    let sharpness = roi_scores.into_iter().fold(f64::INFINITY, f64::min);
    let small = resize_area(&gray, 9, 8);
    let dark_count = gray.pixels().filter(|pixel| pixel.0[0] < 12).count() as f64;
    let bright_count = gray.pixels().filter(|pixel| pixel.0[0] > 245).count() as f64;
    let brightness = gray
        .pixels()
        .map(|pixel| f64::from(pixel.0[0]))
        .sum::<f64>()
        / (u64::from(width) * u64::from(height)) as f64;
    let total = (u64::from(width) * u64::from(height)) as f64;
    let dark_fraction = dark_count / total;
    let bright_fraction = bright_count / total;

    let mut flags = Vec::new();
    if sharpness < blur_threshold {
        flags.push("ぼけ候補".to_owned());
    }
    if dark_fraction > 0.85 {
        flags.push("黒つぶれ候補".to_owned());
    }
    if bright_fraction > 0.95 {
        flags.push("白飛び候補".to_owned());
    }
    if minimum_side < 20 {
        flags.push("対象が小さい".to_owned());
    }

    let dhash = difference_hash(&small);
    let score = flags.len() as f64 * 100.0 + (blur_threshold - sharpness).max(0.0);
    let digest = Sha256::digest(&payload);

    Ok(Quality {
        sha256: format!("{digest:x}"),
        sharpness: round_to_2(sharpness),
        brightness: round_to_2(brightness),
        dark_fraction: round_to_4(dark_fraction),
        bright_fraction: round_to_4(bright_fraction),
        min_target_pixels: minimum_side,
        dhash,
        flags,
        score,
        algorithm_version: QUALITY_ALGORITHM_VERSION.to_owned(),
        parameter_fingerprint: quality_fingerprint(blur_threshold),
    })
}

fn ensure_dimensions(width: u32, height: u32) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("image dimensions must be non-zero");
    }
    if u64::from(width) * u64::from(height) >= (1_u64 << 32) {
        bail!("image has too many pixels for connected-component labeling");
    }
    Ok(())
}

fn rgb_to_gray(rgb: &RgbImage) -> GrayImage {
    GrayImage::from_fn(rgb.width(), rgb.height(), |x, y| {
        let [red, green, blue] = rgb.get_pixel(x, y).0;
        let luminance = (u64::from(red) * GRAY_R
            + u64::from(green) * GRAY_G
            + u64::from(blue) * GRAY_B
            + GRAY_ROUND)
            >> GRAY_SHIFT;
        Luma([luminance.min(255) as u8])
    })
}

fn foreground_ratio(mask: &GrayImage, pixel_count: u64) -> f64 {
    let foreground = mask.pixels().filter(|pixel| pixel.0[0] != 0).count() as f64;
    foreground / pixel_count as f64
}

fn select_components(
    mask: &GrayImage,
    config: &DetectorConfig,
) -> Result<(Option<PixelBox>, usize)> {
    // imageproc's union-find labels its first foreground pixel as 1, which is
    // out of range for a one-pixel image (whose forest contains only index 0).
    if mask.width() == 1 && mask.height() == 1 {
        return Ok(if mask.get_pixel(0, 0).0[0] == 0 {
            (None, 0)
        } else {
            (
                Some(PixelBox {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                }),
                1,
            )
        });
    }

    let labels = connected_components(mask, Connectivity::Eight, Luma([0_u8]));
    let max_label = labels.pixels().map(|pixel| pixel.0[0]).max().unwrap_or(0) as usize;
    if max_label == 0 {
        return Ok((None, 0));
    }

    let mut components = vec![None::<Component>; max_label + 1];
    let image_width = u64::from(mask.width());
    for (x, y, pixel) in labels.enumerate_pixels() {
        let label = pixel.0[0] as usize;
        if label == 0 {
            continue;
        }
        let entry = components[label].get_or_insert(Component {
            bounds: PixelBox {
                x,
                y,
                width: 1,
                height: 1,
            },
            area: 0,
            first_pixel: u64::from(y) * image_width + u64::from(x),
        });
        entry.area += 1;
        let left = entry.bounds.x.min(x);
        let top = entry.bounds.y.min(y);
        let right = entry.bounds.right().max(x + 1);
        let bottom = entry.bounds.bottom().max(y + 1);
        entry.bounds = PixelBox {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        entry.first_pixel = entry
            .first_pixel
            .min(u64::from(y) * image_width + u64::from(x));
    }

    let total_pixels = u64::from(mask.width()) * u64::from(mask.height());
    let min_area = ((total_pixels as f64 * config.min_area_ratio).ceil() as u64).max(1);
    let mut selected_components: Vec<Component> = components
        .into_iter()
        .flatten()
        .filter(|component| component.area >= min_area)
        .collect();
    selected_components.sort_by(|left, right| {
        right
            .area
            .cmp(&left.area)
            .then_with(|| left.first_pixel.cmp(&right.first_pixel))
    });
    if selected_components.is_empty() {
        return Ok((None, 0));
    }

    let main_area = selected_components[0].area as f64;
    let mut selected = vec![selected_components[0].bounds];
    let mut pending: Vec<PixelBox> = selected_components
        .iter()
        .skip(1)
        .filter(|component| component.area as f64 >= main_area * config.merge_component_ratio)
        .map(|component| component.bounds)
        .collect();
    let max_distance = f64::from(mask.width().min(mask.height())) * config.merge_gap_ratio;

    loop {
        let mut additions = Vec::new();
        let mut remaining = Vec::new();
        for candidate in pending {
            if selected
                .iter()
                .any(|existing| box_distance(candidate, *existing) <= max_distance)
            {
                additions.push(candidate);
            } else {
                remaining.push(candidate);
            }
        }
        if additions.is_empty() {
            break;
        }
        selected.extend(additions);
        pending = remaining;
    }

    let left = selected.iter().map(|bbox| bbox.x).min().unwrap_or(0);
    let top = selected.iter().map(|bbox| bbox.y).min().unwrap_or(0);
    let right = selected
        .iter()
        .map(|bbox| bbox.right())
        .max()
        .unwrap_or(left);
    let bottom = selected
        .iter()
        .map(|bbox| bbox.bottom())
        .max()
        .unwrap_or(top);
    Ok((
        Some(PixelBox {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        }),
        selected.len(),
    ))
}

fn box_distance(first: PixelBox, second: PixelBox) -> f64 {
    let dx = (i64::from(first.x) - i64::from(second.right()))
        .max(i64::from(second.x) - i64::from(first.right()))
        .max(0) as f64;
    let dy = (i64::from(first.y) - i64::from(second.bottom()))
        .max(i64::from(second.y) - i64::from(first.bottom()))
        .max(0) as f64;
    dx.hypot(dy)
}

fn validate_box(bbox: &NormalizedBox) -> Result<()> {
    let values = [bbox.cx, bbox.cy, bbox.width, bbox.height];
    if !values.into_iter().all(f64::is_finite)
        || bbox.width <= 0.0
        || bbox.height <= 0.0
        || bbox.cx - bbox.width / 2.0 < 0.0
        || bbox.cy - bbox.height / 2.0 < 0.0
        || bbox.cx + bbox.width / 2.0 > 1.0
        || bbox.cy + bbox.height / 2.0 > 1.0
    {
        bail!("box must have finite positive dimensions within the image");
    }
    Ok(())
}

fn normalized_floor(value: f64, extent: u32) -> u32 {
    (value * f64::from(extent)).trunc().max(0.0) as u32
}

fn normalized_ceil(value: f64, extent: u32) -> u32 {
    (value * f64::from(extent)).ceil().max(0.0) as u32
}

fn crop_gray(image: &GrayImage, x: u32, y: u32, width: u32, height: u32) -> GrayImage {
    GrayImage::from_fn(width, height, |dx, dy| *image.get_pixel(x + dx, y + dy))
}

/// Resize by integrating the piecewise-constant source pixel areas.
///
/// OpenCV's INTER_AREA uses area averaging for shrink operations and behaves
/// like a nearest-neighbor enlargement. This implementation keeps that
/// deterministic area interpretation in both directions; metrics are tagged
/// with an algorithm version so cached values are never silently mixed.
fn resize_area(image: &GrayImage, out_width: u32, out_height: u32) -> GrayImage {
    let in_width = image.width();
    let in_height = image.height();
    let scale_x = f64::from(in_width) / f64::from(out_width);
    let scale_y = f64::from(in_height) / f64::from(out_height);
    let divisor = scale_x * scale_y;
    GrayImage::from_fn(out_width, out_height, |dx, dy| {
        let x0 = f64::from(dx) * scale_x;
        let x1 = f64::from(dx + 1) * scale_x;
        let y0 = f64::from(dy) * scale_y;
        let y1 = f64::from(dy + 1) * scale_y;
        let sx0 = x0.floor() as u32;
        let sx1 = x1.ceil().min(f64::from(in_width)) as u32;
        let sy0 = y0.floor() as u32;
        let sy1 = y1.ceil().min(f64::from(in_height)) as u32;
        let mut sum = 0.0;
        for sy in sy0..sy1 {
            let wy = (y1.min(f64::from(sy + 1)) - y0.max(f64::from(sy))).max(0.0);
            for sx in sx0..sx1 {
                let wx = (x1.min(f64::from(sx + 1)) - x0.max(f64::from(sx))).max(0.0);
                sum += f64::from(image.get_pixel(sx, sy).0[0]) * wx * wy;
            }
        }
        let value = (sum / divisor).round().clamp(0.0, 255.0) as u8;
        Luma([value])
    })
}

fn reflect101(position: i64, length: u32) -> u32 {
    if length <= 1 {
        return 0;
    }
    let length = i64::from(length);
    let mut position = position;
    while position < 0 || position >= length {
        if position < 0 {
            position = -position;
        } else {
            position = 2 * length - position - 2;
        }
    }
    position as u32
}

fn laplacian_variance(image: &GrayImage) -> f64 {
    let width = image.width();
    let height = image.height();
    let mut sum = 0.0;
    let mut sum_squares = 0.0;
    let count = f64::from(width) * f64::from(height);
    for y in 0..height {
        for x in 0..width {
            let x = i64::from(x);
            let y = i64::from(y);
            let center = f64::from(image.get_pixel(x as u32, y as u32).0[0]);
            let north = f64::from(image.get_pixel(x as u32, reflect101(y - 1, height)).0[0]);
            let south = f64::from(image.get_pixel(x as u32, reflect101(y + 1, height)).0[0]);
            let west = f64::from(image.get_pixel(reflect101(x - 1, width), y as u32).0[0]);
            let east = f64::from(image.get_pixel(reflect101(x + 1, width), y as u32).0[0]);
            let value = north + south + west + east - 4.0 * center;
            sum += value;
            sum_squares += value * value;
        }
    }
    (sum_squares / count - (sum / count).powi(2)).max(0.0)
}

fn difference_hash(image: &GrayImage) -> String {
    let mut result = String::with_capacity(64);
    for y in 0..image.height() {
        for x in 0..image.width().saturating_sub(1) {
            let left = image.get_pixel(x, y).0[0];
            let right = image.get_pixel(x + 1, y).0[0];
            result.push(if right > left { '1' } else { '0' });
        }
    }
    result
}

/// Return a stable fingerprint for the quality settings represented by the
/// current algorithm. The algorithm version is stored separately.
pub fn quality_fingerprint(blur_threshold: f64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"labeler-vision-quality-parameters-v1\0");
    hasher.update(blur_threshold.to_bits().to_be_bytes());
    let digest = hasher.finalize();
    format!("{digest:x}")
}

fn round_to_2(value: f64) -> f64 {
    (value * 100.0).round_ties_even() / 100.0
}

fn round_to_4(value: f64) -> f64 {
    (value * 10_000.0).round_ties_even() / 10_000.0
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb, RgbImage};
    use sha2::{Digest, Sha256};

    use super::{
        DetectorConfig, NormalizedBox, QUALITY_ALGORITHM_VERSION, detect, quality,
        quality_fingerprint,
    };

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn rgb_image(width: u32, height: u32, fill: [u8; 3]) -> RgbImage {
        ImageBuffer::from_pixel(width, height, Rgb(fill))
    }

    fn temp_png(image: &RgbImage) -> PathBuf {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "labeler-vision-{}-{sequence}.png",
            std::process::id()
        ));
        DynamicImage::ImageRgb8(image.clone())
            .save_with_format(&path, ImageFormat::Png)
            .expect("write PNG fixture");
        path
    }

    #[test]
    fn detector_defaults_match_the_python_backend() {
        let config = DetectorConfig::default();
        assert_eq!(config.threshold, None);
        assert_eq!(config.min_area_ratio, 0.001);
        assert_eq!(config.merge_gap_ratio, 0.20);
        assert_eq!(config.merge_component_ratio, 0.05);
        assert_eq!(config.padding, 0);
    }

    #[test]
    fn manual_threshold_includes_equal_pixels_and_rgb_uses_opencv_gray_weights() {
        let image = DynamicImage::ImageRgb8(rgb_image(1, 1, [255, 0, 0]));
        let config = DetectorConfig {
            threshold: Some(76),
            ..DetectorConfig::default()
        };
        let result = detect(&image, &config).expect("red pixel should be readable");
        assert_eq!(result.method, "manual");
        assert_eq!(result.threshold, 76);
        assert_eq!(result.boxes.len(), 1);
        assert_eq!(result.component_count, 1);
        assert_eq!(result.boxes[0].class_id, 0);
        assert_eq!(result.boxes[0].width, 1.0);
        assert_eq!(result.boxes[0].height, 1.0);
        assert!(result.flags.contains(&"touches_border".to_owned()));

        let white = DynamicImage::ImageRgb8(rgb_image(1, 1, [255, 255, 255]));
        let empty = detect(
            &white,
            &DetectorConfig {
                threshold: Some(0),
                ..DetectorConfig::default()
            },
        )
        .expect("one-pixel background should be safe");
        assert!(empty.boxes.is_empty());
    }

    #[test]
    fn otsu_finds_black_symbol_and_returns_normalized_coordinates() {
        let mut image = rgb_image(64, 64, [255, 255, 255]);
        for y in 10..20 {
            for x in 12..28 {
                image.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let result = detect(&DynamicImage::ImageRgb8(image), &DetectorConfig::default())
            .expect("detect black symbol");
        assert_eq!(result.method, "otsu");
        assert_eq!(result.threshold, 0);
        assert_eq!(result.component_count, 1);
        assert_eq!(result.boxes.len(), 1);
        assert!((result.boxes[0].cx - 20.0 / 64.0).abs() < 1e-12);
        assert!((result.boxes[0].cy - 15.0 / 64.0).abs() < 1e-12);
        assert!((result.boxes[0].width - 16.0 / 64.0).abs() < 1e-12);
        assert!((result.boxes[0].height - 10.0 / 64.0).abs() < 1e-12);
        assert_eq!(result.foreground_ratio, 160.0 / 4096.0);
    }

    #[test]
    fn eight_connected_pixels_form_one_component_and_nearby_component_merges() {
        let mut diagonal = rgb_image(8, 8, [255, 255, 255]);
        diagonal.put_pixel(1, 1, Rgb([0, 0, 0]));
        diagonal.put_pixel(2, 2, Rgb([0, 0, 0]));
        let diagonal_result = detect(
            &DynamicImage::ImageRgb8(diagonal),
            &DetectorConfig {
                min_area_ratio: 0.01,
                ..DetectorConfig::default()
            },
        )
        .expect("eight-connected labeling");
        assert_eq!(diagonal_result.component_count, 1);
        assert_eq!(diagonal_result.boxes[0].width, 2.0 / 8.0);
        assert_eq!(diagonal_result.boxes[0].height, 2.0 / 8.0);

        let mut nearby = rgb_image(100, 100, [255, 255, 255]);
        for y in 10..20 {
            for x in 10..20 {
                nearby.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        for y in 12..15 {
            for x in 22..25 {
                nearby.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }
        let merged = detect(
            &DynamicImage::ImageRgb8(nearby),
            &DetectorConfig {
                min_area_ratio: 0.0001,
                merge_gap_ratio: 0.02,
                ..DetectorConfig::default()
            },
        )
        .expect("merge nearby components");
        assert_eq!(merged.component_count, 2);
        assert!(merged.flags.contains(&"merged_components".to_owned()));
        assert!((merged.boxes[0].width - 15.0 / 100.0).abs() < 1e-12);
        assert!((merged.boxes[0].height - 10.0 / 100.0).abs() < 1e-12);
    }

    #[test]
    fn otsu_fallback_uses_rgb_value_and_reports_flags() {
        let mut image = rgb_image(20, 20, [0, 0, 0]);
        image.put_pixel(10, 10, Rgb([255, 255, 255]));
        let result = detect(&DynamicImage::ImageRgb8(image), &DetectorConfig::default())
            .expect("fallback detection");
        assert_eq!(result.method, "hsv_v60");
        assert_eq!(result.threshold, 60);
        assert!(result.flags.contains(&"fallback_used".to_owned()));
        assert!(result.flags.contains(&"large_foreground".to_owned()));
        assert!(result.flags.contains(&"large_box".to_owned()));
        assert!(result.flags.contains(&"touches_border".to_owned()));
        assert_eq!(result.boxes[0].width, 1.0);
        assert_eq!(result.boxes[0].height, 1.0);
    }

    #[test]
    fn empty_light_image_produces_no_detection() {
        let image = DynamicImage::ImageRgb8(rgb_image(32, 24, [255, 255, 255]));
        let result = detect(&image, &DetectorConfig::default()).expect("empty image");
        assert_eq!(result.method, "otsu");
        assert_eq!(result.threshold, 0);
        assert_eq!(result.boxes.len(), 0);
        assert_eq!(result.component_count, 0);
        assert_eq!(result.foreground_ratio, 0.0);
        assert!(result.flags.is_empty());
    }

    #[test]
    fn detector_rejects_non_finite_or_out_of_range_ratios() {
        let image = DynamicImage::ImageRgb8(rgb_image(8, 8, [255, 255, 255]));
        for config in [
            DetectorConfig {
                min_area_ratio: f64::NAN,
                ..DetectorConfig::default()
            },
            DetectorConfig {
                merge_gap_ratio: f64::INFINITY,
                ..DetectorConfig::default()
            },
            DetectorConfig {
                merge_component_ratio: -0.01,
                ..DetectorConfig::default()
            },
            DetectorConfig {
                min_area_ratio: 1.01,
                ..DetectorConfig::default()
            },
        ] {
            assert!(detect(&image, &config).is_err());
        }
    }

    #[test]
    fn quality_reports_hash_metrics_flags_and_parameter_version() {
        let image = rgb_image(40, 30, [128, 128, 128]);
        let path = temp_png(&image);
        let expected_hash = format!("{:x}", Sha256::digest(fs::read(&path).expect("read PNG")));
        let bbox = NormalizedBox {
            class_id: 0,
            cx: 0.5,
            cy: 0.5,
            width: 0.5,
            height: 0.5,
        };
        let result = quality(&path, &[bbox], 45.0).expect("quality metrics");
        fs::remove_file(&path).expect("remove PNG fixture");

        assert_eq!(result.sha256, expected_hash);
        assert_eq!(result.sharpness, 0.0);
        assert_eq!(result.brightness, 128.0);
        assert_eq!(result.dark_fraction, 0.0);
        assert_eq!(result.bright_fraction, 0.0);
        assert_eq!(result.min_target_pixels, 16);
        assert_eq!(result.dhash, "0".repeat(64));
        assert_eq!(result.flags, vec!["ぼけ候補", "対象が小さい"]);
        assert_eq!(result.score, 245.0);
        assert_eq!(result.algorithm_version, QUALITY_ALGORITHM_VERSION);
        assert_eq!(result.parameter_fingerprint.len(), 64);
        assert_eq!(result.parameter_fingerprint, quality_fingerprint(45.0));
    }

    #[test]
    fn quality_difference_hash_and_lighting_thresholds_are_stable() {
        let mut gradient = RgbImage::new(9, 8);
        for y in 0..8 {
            for x in 0..9 {
                let value = x as u8;
                gradient.put_pixel(x, y, Rgb([value, value, value]));
            }
        }
        let path = temp_png(&gradient);
        let result = quality(&path, &[], 45.0).expect("gradient quality");
        fs::remove_file(&path).expect("remove PNG fixture");
        assert_eq!(result.dhash, "1".repeat(64));
        assert_eq!(
            result.flags,
            vec!["ぼけ候補", "黒つぶれ候補", "対象が小さい"]
        );

        let dark_path = temp_png(&rgb_image(20, 20, [11, 11, 11]));
        let dark = quality(&dark_path, &[], 45.0).expect("dark quality");
        fs::remove_file(&dark_path).expect("remove dark PNG fixture");
        assert_eq!(dark.brightness, 11.0);
        assert_eq!(dark.dark_fraction, 1.0);
        assert!(dark.flags.contains(&"黒つぶれ候補".to_owned()));

        let bright_path = temp_png(&rgb_image(20, 20, [246, 246, 246]));
        let bright = quality(&bright_path, &[], 45.0).expect("bright quality");
        fs::remove_file(&bright_path).expect("remove bright PNG fixture");
        assert_eq!(bright.brightness, 246.0);
        assert_eq!(bright.bright_fraction, 1.0);
        assert!(bright.flags.contains(&"白飛び候補".to_owned()));
    }

    #[test]
    fn quality_rejects_non_finite_parameters_and_invalid_boxes() {
        let path = temp_png(&rgb_image(8, 8, [120, 120, 120]));
        assert!(quality(&path, &[], f64::NAN).is_err());
        let invalid = NormalizedBox {
            class_id: 0,
            cx: 0.5,
            cy: 0.5,
            width: f64::INFINITY,
            height: 0.5,
        };
        assert!(quality(&path, &[invalid], 45.0).is_err());
        fs::remove_file(path).expect("remove PNG fixture");
    }
}
