use crate::NormalizedBox;
use image::{Rgb, RgbImage, imageops};
use rand::{RngExt, SeedableRng, rngs::StdRng};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct AugmentOptions {
    pub copies: u8,
    pub brightness: bool,
    pub contrast: bool,
    pub hue: bool,
    pub zoom_out: bool,
    pub crop: bool,
    pub rotate: bool,
    pub perspective: bool,
    pub blur: bool,
    pub noise: bool,
}

impl Default for AugmentOptions {
    fn default() -> Self {
        Self {
            copies: 1,
            brightness: true,
            contrast: true,
            hue: true,
            zoom_out: true,
            crop: true,
            rotate: true,
            perspective: true,
            blur: true,
            noise: true,
        }
    }
}

impl AugmentOptions {
    pub fn any_effect(self) -> bool {
        self.brightness
            || self.contrast
            || self.hue
            || self.zoom_out
            || self.crop
            || self.rotate
            || self.perspective
            || self.blur
            || self.noise
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Applied {
    /// Scale and offset map source pixel boundaries to destination boundaries before perspective and rotation.
    pub scale: Option<f64>,
    pub offset: Option<[f64; 2]>,
    pub rotation_degrees: Option<f64>,
    /// Strength of the top-edge-narrowing trapezoid, applied before rotation.
    pub perspective_strength: Option<f64>,
    pub brightness_factor: Option<f64>,
    pub contrast: Option<f64>,
    pub hue_degrees: Option<i32>,
    pub blur_sigma: Option<f32>,
    pub noise_limit: Option<i8>,
    pub crop_fallback: bool,
    pub rotation_skipped: bool,
}

#[derive(Clone, Copy)]
struct Geometry {
    scale: f64,
    x: f64,
    y: f64,
    angle: f64,
    perspective: f64,
}

impl Geometry {
    // Coordinates are relative to the canvas center. The top edge narrows and
    // the bottom edge stays full width; projective lines remain straight.
    fn project(self, x: f64, y: f64, half_height: f64) -> (f64, f64) {
        let strength = self.perspective;
        let denominator = 1. - strength * (y / half_height);
        (
            x * (1. - strength) / denominator,
            (y - strength * half_height) / denominator,
        )
    }

    fn unproject(self, x: f64, y: f64, half_height: f64) -> (f64, f64) {
        let strength = self.perspective;
        let source_y = (y + strength * half_height) / (1. + strength * (y / half_height));
        let source_x = x * (1. - strength * (source_y / half_height)) / (1. - strength);
        (source_x, source_y)
    }

    fn boxes(self, boxes: &[NormalizedBox], w: f64, h: f64) -> Option<Vec<NormalizedBox>> {
        let (sin, cos) = self.angle.to_radians().sin_cos();
        boxes
            .iter()
            .map(|b| {
                let mut left = f64::INFINITY;
                let mut top = f64::INFINITY;
                let mut right = f64::NEG_INFINITY;
                let mut bottom = f64::NEG_INFINITY;
                for dx in [-0.5, 0.5] {
                    for dy in [-0.5, 0.5] {
                        let x = (b.cx + b.width * dx) * w * self.scale + self.x - w / 2.;
                        let y = (b.cy + b.height * dy) * h * self.scale + self.y - h / 2.;
                        let (x, y) = self.project(x, y, h / 2.);
                        let px = x * cos - y * sin + w / 2.;
                        let py = x * sin + y * cos + h / 2.;
                        left = left.min(px);
                        right = right.max(px);
                        top = top.min(py);
                        bottom = bottom.max(py);
                    }
                }
                if left < -1e-9 || top < -1e-9 || right > w + 1e-9 || bottom > h + 1e-9 {
                    return None;
                }
                let (l, r) = (left.clamp(0., w), right.clamp(0., w));
                let (t, btm) = (top.clamp(0., h), bottom.clamp(0., h));
                Some(NormalizedBox {
                    class_id: b.class_id,
                    cx: (l + r) / (2. * w),
                    cy: (t + btm) / (2. * h),
                    width: (r - l) / w,
                    height: (btm - t) / h,
                })
            })
            .collect()
    }

    fn render(self, source: &RgbImage) -> RgbImage {
        let (w, h) = (source.width() as f64, source.height() as f64);
        let (sin, cos) = self.angle.to_radians().sin_cos();
        RgbImage::from_fn(source.width(), source.height(), |x, y| {
            let x = f64::from(x) + 0.5 - w / 2.;
            let y = f64::from(y) + 0.5 - h / 2.;
            let x_rotated = x * cos + y * sin;
            let y_rotated = -x * sin + y * cos;
            if x_rotated.abs() >= w / 2. || y_rotated.abs() >= h / 2. {
                return Rgb([128; 3]);
            }
            let (x, y) = if self.perspective == 0. {
                (x_rotated, y_rotated)
            } else {
                self.unproject(x_rotated, y_rotated, h / 2.)
            };
            if x.abs() > w / 2. || y.abs() > h / 2. {
                return Rgb([128; 3]);
            }
            let (px, py) = (x + w / 2., y + h / 2.);
            let sx = ((px - self.x) / self.scale - 0.5).clamp(0., w - 1.);
            let sy = ((py - self.y) / self.scale - 0.5).clamp(0., h - 1.);
            let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
            let (x1, y1) = (
                (x0 + 1).min(source.width() - 1),
                (y0 + 1).min(source.height() - 1),
            );
            let (fx, fy) = (sx.fract(), sy.fract());
            let lerp = |a, b, t| a * (1. - t) + b * t;
            Rgb(std::array::from_fn(|ch| {
                let top = lerp(
                    f64::from(source[(x0, y0)][ch]),
                    f64::from(source[(x1, y0)][ch]),
                    fx,
                );
                let bottom = lerp(
                    f64::from(source[(x0, y1)][ch]),
                    f64::from(source[(x1, y1)][ch]),
                    fx,
                );
                (lerp(top, bottom, fy) as f64).round() as u8
            }))
        })
    }
}

/// Generates one copy directly from the source. Seeded per export candidate.
pub fn generate(
    source: &RgbImage,
    boxes: &[NormalizedBox],
    options: AugmentOptions,
    seed: u64,
) -> (Option<(RgbImage, Vec<NormalizedBox>)>, Applied) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut applied = Applied::default();
    let mut geometry = Geometry {
        scale: 1.,
        x: 0.,
        y: 0.,
        angle: 0.,
        perspective: 0.,
    };
    let (w, h) = (f64::from(source.width()), f64::from(source.height()));
    let try_crop = options.crop && (!options.zoom_out || rng.random_bool(0.5));
    if try_crop {
        for _ in 0..8 {
            let fraction = rng.random_range(0.85..=0.95);
            let crop = Geometry {
                scale: 1. / fraction,
                x: -w * rng.random_range(0. ..=1. - fraction) / fraction,
                y: -h * rng.random_range(0. ..=1. - fraction) / fraction,
                ..geometry
            };
            if crop.boxes(boxes, w, h).is_some() {
                geometry = crop;
                break;
            }
        }
        applied.crop_fallback = geometry.scale == 1.;
    }
    if options.zoom_out && geometry.scale == 1. {
        geometry.scale = rng.random_range(0.85..=0.95);
        geometry.x = w * (1. - geometry.scale) / 2.;
        geometry.y = h * (1. - geometry.scale) / 2.;
    }
    if geometry.scale != 1. {
        applied.scale = Some(geometry.scale);
        applied.offset = Some([geometry.x, geometry.y]);
    }
    if options.perspective {
        geometry.perspective = rng.random_range(0.1..=0.2);
        applied.perspective_strength = Some(geometry.perspective);
    }
    let mut mapped = if applied.scale.is_some() || applied.perspective_strength.is_some() {
        geometry
            .boxes(boxes, w, h)
            .expect("selected geometry contains all boxes")
    } else {
        boxes.to_vec()
    };
    if options.rotate {
        for _ in 0..8 {
            let angle = rng.random_range(-45.0_f64..=45.);
            if angle.abs() < 0.000001 {
                continue;
            }
            let rotated = Geometry { angle, ..geometry };
            if let Some(rotated_boxes) = rotated.boxes(boxes, w, h) {
                geometry = rotated;
                mapped = rotated_boxes;
                applied.rotation_degrees = Some(angle);
                break;
            }
        }
        applied.rotation_skipped = applied.rotation_degrees.is_none();
    }
    applied.brightness_factor = options.brightness.then(|| rng.random_range(0.8..=1.2));
    applied.contrast = options.contrast.then(|| rng.random_range(0.8..=1.2));
    applied.hue_degrees = options
        .hue
        .then(|| rng.random_range(-5..5))
        .map(|v| if v >= 0 { v + 1 } else { v });
    let mut blur = options.blur && rng.random_bool(0.25);
    let mut noise = options.noise && rng.random_bool(0.25);
    let geometric = applied.scale.is_some()
        || applied.perspective_strength.is_some()
        || applied.rotation_degrees.is_some();
    if !geometric && !options.brightness && !options.contrast && !options.hue && !blur && !noise {
        if options.blur {
            blur = true;
        } else if options.noise {
            noise = true;
        } else {
            return (None, applied);
        }
    }
    let mut output = if geometric {
        geometry.render(source)
    } else {
        source.clone()
    };
    if options.contrast || options.brightness {
        let contrast = applied.contrast.unwrap_or(1.);
        let brightness = applied.brightness_factor.unwrap_or(1.);
        for pixel in output.pixels_mut() {
            for value in &mut pixel.0 {
                *value = (((f64::from(*value) - 127.5) * contrast + 127.5) * brightness)
                    .round()
                    .clamp(0., 255.) as u8;
            }
        }
    }
    if let Some(degrees) = applied.hue_degrees {
        output = imageops::huerotate(&output, degrees);
    }
    if blur {
        let sigma = rng.random_range(0.3..=0.8);
        output = imageops::blur(&output, sigma);
        applied.blur_sigma = Some(sigma);
    }
    if noise {
        applied.noise_limit = Some(4);
        for pixel in output.pixels_mut() {
            for value in &mut pixel.0 {
                *value = (i16::from(*value) + rng.random_range(-4..=4)).clamp(0, 255) as u8;
            }
        }
    }
    if output == *source && mapped == boxes {
        return (None, applied);
    }
    (Some((output, mapped)), applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only_geometry() -> AugmentOptions {
        AugmentOptions {
            brightness: false,
            contrast: false,
            hue: false,
            blur: false,
            noise: false,
            zoom_out: false,
            crop: false,
            rotate: false,
            perspective: false,
            ..Default::default()
        }
    }

    #[test]
    fn seeded_rotation_is_bounded_and_encloses_all_four_corners() {
        let source = RgbImage::from_fn(100, 80, |x, y| Rgb([x as u8, y as u8, 10]));
        let boxes = [NormalizedBox {
            class_id: 2,
            cx: 0.5,
            cy: 0.5,
            width: 0.3,
            height: 0.2,
        }];
        let settings = AugmentOptions {
            rotate: true,
            ..only_geometry()
        };
        let mut exceeds_old_limit = false;
        for seed in 0..30 {
            let (copy, applied) = generate(&source, &boxes, settings, seed);
            assert_eq!(copy, generate(&source, &boxes, settings, seed).0);
            let angle = applied.rotation_degrees.unwrap();
            assert!(angle.abs() <= 45.);
            exceeds_old_limit |= angle.abs() > 10.;
            let (image, mapped) = copy.unwrap();
            assert_eq!(image.dimensions(), source.dimensions());
            let (sin, cos) = angle.to_radians().sin_cos();
            assert_eq!(mapped[0].class_id, 2);
            assert!((mapped[0].cx - 0.5).abs() < 1e-9);
            assert!((mapped[0].cy - 0.5).abs() < 1e-9);
            assert!((mapped[0].width - (30. * cos.abs() + 16. * sin.abs()) / 100.).abs() < 1e-9);
            assert!((mapped[0].height - (30. * sin.abs() + 16. * cos.abs()) / 80.).abs() < 1e-9);
        }
        assert!(exceeds_old_limit);
    }

    #[test]
    fn perspective_narrows_top_edge_and_transforms_labels_with_the_pixels() {
        let source = RgbImage::from_fn(101, 81, |x, _| {
            if x < 50 {
                Rgb([200, 0, 0])
            } else {
                Rgb([0, 0, 200])
            }
        });
        let boxes = [NormalizedBox {
            class_id: 1,
            cx: 0.3,
            cy: 0.25,
            width: 0.4,
            height: 0.5,
        }];
        let settings = AugmentOptions {
            perspective: true,
            ..only_geometry()
        };
        assert!(settings.any_effect());
        for seed in 0..8 {
            let (copy, applied) = generate(&source, &boxes, settings, seed);
            assert_eq!(copy, generate(&source, &boxes, settings, seed).0);
            let strength = applied.perspective_strength.unwrap();
            assert!((0.1..=0.2).contains(&strength));
            let (image, mapped) = copy.unwrap();
            assert_eq!(image.dimensions(), source.dimensions());
            assert_eq!(image[(0, 0)], Rgb([128; 3]));
            assert_eq!(image[(10, 72)], Rgb([200, 0, 0]));
            assert_eq!(image[(90, 72)], Rgb([0, 0, 200]));
            let left = 0.5 - 0.4 * (1. - strength);
            let bottom = (1. - strength) / 2.;
            assert_eq!(mapped[0].class_id, 1);
            assert!((mapped[0].cx - (left + 0.5) / 2.).abs() < 1e-9);
            assert!((mapped[0].cy - bottom / 2.).abs() < 1e-9);
            assert!((mapped[0].width - (0.5 - left)).abs() < 1e-9);
            assert!((mapped[0].height - bottom).abs() < 1e-9);
        }
    }

    #[test]
    fn brightness_and_contrast_use_multipliers_within_twenty_percent() {
        let source = RgbImage::from_pixel(1, 1, Rgb([60, 100, 180]));
        let settings = AugmentOptions {
            brightness: true,
            contrast: true,
            ..only_geometry()
        };
        for seed in 0..32 {
            let (copy, applied) = generate(&source, &[], settings, seed);
            let brightness = applied.brightness_factor.unwrap();
            let contrast = applied.contrast.unwrap();
            assert!((0.8..=1.2).contains(&brightness));
            assert!((0.8..=1.2).contains(&contrast));
            let expected = Rgb([60., 100., 180.]
                .map(|value| (((value - 127.5) * contrast + 127.5) * brightness).round() as u8));
            if expected == source[(0, 0)] {
                assert!(copy.is_none());
            } else {
                assert_eq!(copy.unwrap().0[(0, 0)], expected);
            }
        }
    }

    #[test]
    fn full_frame_box_cannot_be_cropped_or_rotated_without_zooming_out() {
        let source = RgbImage::from_pixel(40, 30, Rgb([40, 10, 20]));
        let boxes = [NormalizedBox {
            class_id: 0,
            cx: 0.5,
            cy: 0.5,
            width: 1.,
            height: 1.,
        }];
        let options = AugmentOptions {
            crop: true,
            rotate: true,
            ..only_geometry()
        };
        let (copy, applied) = generate(&source, &boxes, options, 3);
        assert!(copy.is_none() && applied.crop_fallback && applied.rotation_skipped);

        let options = AugmentOptions {
            zoom_out: true,
            rotate: false,
            ..options
        };
        let (copy, applied) = (0..20)
            .map(|seed| generate(&source, &boxes, options, seed))
            .find(|(_, applied)| applied.crop_fallback)
            .unwrap();
        let (_, boxes) = copy.unwrap();
        assert!((0.85..=0.95).contains(&boxes[0].width));
        assert!((boxes[0].width - applied.scale.unwrap()).abs() < 1e-9);
        assert!((boxes[0].cx - 0.5).abs() < 1e-9);
    }
}
