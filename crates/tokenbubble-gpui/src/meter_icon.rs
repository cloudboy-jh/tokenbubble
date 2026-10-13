//! Rasterizes the tray meter: up to two pill-shaped bars (session above weekly), drawn in a
//! single color that contrasts with the taskbar. Pure so it is testable off-platform.

use crate::presentation::TrayMeter;

const SUBSAMPLES: u32 = 4;
const TRACK_ALPHA: f32 = 0.35;
const DIMMED_TRACK_ALPHA: f32 = 0.25;
const FILL_ALPHA: f32 = 1.0;
const DIMMED_FILL_ALPHA: f32 = 0.55;

/// Foreground for each taskbar theme: white on dark, near-black on light.
pub fn foreground(light_taskbar: bool) -> [u8; 3] {
    if light_taskbar {
        [0x1c, 0x1c, 0x1c]
    } else {
        [0xff, 0xff, 0xff]
    }
}

struct Pill {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    /// Fill extends from `left` to here.
    fill_right: f32,
}

impl Pill {
    fn contains(&self, x: f32, y: f32) -> bool {
        let radius = (self.bottom - self.top) / 2.0;
        let center_y = (self.top + self.bottom) / 2.0;
        let center_x = x.clamp(self.left + radius, self.right - radius);
        let (dx, dy) = (x - center_x, y - center_y);
        dx * dx + dy * dy <= radius * radius
    }
}

/// `size × size` pixels, top-down rows, BGRA with straight (non-premultiplied) alpha.
pub fn render(size: u32, meter: &TrayMeter, light_taskbar: bool) -> Vec<u8> {
    let s = size as f32;
    let (left, right) = (s * 0.125, s * 0.875);
    let width = right - left;
    // No reading still draws two empty tracks so the icon keeps its shape.
    let fills: Vec<Option<f32>> = if meter.bars.is_empty() {
        vec![None, None]
    } else {
        meter.bars.iter().copied().map(Some).collect()
    };
    let (height, gap) = if fills.len() == 1 {
        (s * 0.375, 0.0)
    } else {
        (s * 0.25, s * 0.125)
    };
    let total = height * fills.len() as f32 + gap * (fills.len() as f32 - 1.0);
    let pills: Vec<Pill> = fills
        .iter()
        .enumerate()
        .map(|(index, fill)| {
            let top = (s - total) / 2.0 + index as f32 * (height + gap);
            let fraction = fill.unwrap_or(0.0).clamp(0.0, 1.0);
            // Any nonzero reading shows at least a dot.
            let fill_right = if fraction > 0.0 {
                (left + fraction * width).max(left + height * 0.5)
            } else {
                left
            };
            Pill {
                left,
                right,
                top,
                bottom: top + height,
                fill_right,
            }
        })
        .collect();

    let (track_alpha, fill_alpha) = if meter.dimmed {
        (DIMMED_TRACK_ALPHA, DIMMED_FILL_ALPHA)
    } else {
        (TRACK_ALPHA, FILL_ALPHA)
    };
    let [red, green, blue] = foreground(light_taskbar);
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    let step = 1.0 / SUBSAMPLES as f32;
    let samples = (SUBSAMPLES * SUBSAMPLES) as f32;
    for row in 0..size {
        for column in 0..size {
            let (mut track, mut fill) = (0.0f32, 0.0f32);
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let x = column as f32 + (sx as f32 + 0.5) * step;
                    let y = row as f32 + (sy as f32 + 0.5) * step;
                    if let Some(pill) = pills.iter().find(|pill| pill.contains(x, y)) {
                        if x <= pill.fill_right {
                            fill += 1.0;
                        } else {
                            track += 1.0;
                        }
                    }
                }
            }
            let alpha = (fill * fill_alpha + track * track_alpha) / samples;
            let offset = ((row * size + column) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[
                blue,
                green,
                red,
                (alpha * 255.0).round() as u8,
            ]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn two_bars_fill_proportionally_with_faint_tracks() {
        let meter = TrayMeter {
            bars: vec![0.5, 1.0],
            dimmed: false,
        };
        let size = 32;
        let pixels = render(size, &meter, false);
        assert_eq!(pixels.len(), (size * size * 4) as usize);

        // At 32 px the bars span rows 6..14 and 18..26, columns 4..28.
        let (top, bottom) = (10, 22);
        assert_eq!(alpha(&pixels, size, 10, top), 255, "filled half");
        assert_eq!(alpha(&pixels, size, 22, top), 89, "unfilled track");
        assert_eq!(alpha(&pixels, size, 22, bottom), 255, "full bar");
        assert_eq!(alpha(&pixels, size, 16, 2), 0, "outside the bars");
        assert_eq!(alpha(&pixels, size, 4, 6), 0, "rounded corner");
        assert_eq!(&pixels[..3], &[0xff, 0xff, 0xff], "white on a dark taskbar");
    }

    #[test]
    fn stale_or_missing_readings_are_faded_and_keep_their_shape() {
        let size = 32;
        let fresh = render(
            size,
            &TrayMeter {
                bars: vec![1.0],
                dimmed: false,
            },
            true,
        );
        let stale = render(
            size,
            &TrayMeter {
                bars: vec![1.0],
                dimmed: true,
            },
            true,
        );
        assert_eq!(alpha(&fresh, size, 16, 16), 255);
        assert_eq!(alpha(&stale, size, 16, 16), 140);
        assert_eq!(&fresh[..3], &[0x1c, 0x1c, 0x1c], "dark on a light taskbar");

        let empty = render(
            size,
            &TrayMeter {
                bars: Vec::new(),
                dimmed: true,
            },
            false,
        );
        assert_eq!(alpha(&empty, size, 16, 10), 64, "track only");
        assert_eq!(alpha(&empty, size, 16, 22), 64, "second track");
    }

    #[test]
    fn tiny_nonzero_usage_is_still_visible() {
        let size = 16;
        let pixels = render(
            size,
            &TrayMeter {
                bars: vec![0.01, 0.0],
                dimmed: false,
            },
            false,
        );
        // At 16 px the bars span rows 3..7 and 9..13: the top bar's cap is filled, the
        // empty bar's cap is only track.
        assert_eq!(alpha(&pixels, size, 3, 5), 255);
        assert!(alpha(&pixels, size, 3, 11) < 100);
    }
}
