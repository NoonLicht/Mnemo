//! Page tilt (`pipeline.estimate_skew`, `pipeline.rotate_page`): the angle
//! whose horizontal ink profile changes most sharply row to row is the one
//! that levels the text rows.

use super::imageops::{Gray, resize};

// Everything here is sequential on purpose. It runs while the OCR engine mutex is held;
// a rayon parallel iterator would let the waiting worker steal another extraction job
// that asks for the same mutex, which deadlocks the whole index. Images are already
// processed in parallel by the extraction workers.

/// Degrees of tilt searched (quarter turns cover the rest).
const DESKEW_MAX: f64 = 45.0;
/// Smaller tilts are left alone (the search step is 0.1).
pub const DESKEW_MIN: f64 = 0.3;

/// Rotates by `degrees` counter-clockwise about the centre, same size, like
/// `cv2.warpAffine(getRotationMatrix2D((w/2, h/2), degrees, 1))` with
/// bilinear sampling. `replicate`: edge pixels fill the corners (a page);
/// otherwise 0 (an ink map).
pub fn rotate(gray: &Gray, degrees: f64, replicate: bool) -> Gray {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (cx, cy) = (gray.w as f64 / 2.0, gray.h as f64 / 2.0);
    let (wm, hm) = (gray.w as i64 - 1, gray.h as i64 - 1);
    let mut out = Gray::new(gray.w, gray.h, 0);
    out.data
        .chunks_mut(gray.w)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = y as f64 - cy;
            for (x, px) in row.iter_mut().enumerate() {
                let dx = x as f64 - cx;
                let sx = cos * dx - sin * dy + cx;
                let sy = sin * dx + cos * dy + cy;
                let (x0, y0) = (sx.floor() as i64, sy.floor() as i64);
                let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
                let sample = |x: i64, y: i64| -> f64 {
                    if replicate {
                        gray.data[(y.clamp(0, hm) * gray.w as i64 + x.clamp(0, wm)) as usize] as f64
                    } else if x < 0 || y < 0 || x > wm || y > hm {
                        0.0
                    } else {
                        gray.data[(y * gray.w as i64 + x) as usize] as f64
                    }
                };
                let top = sample(x0, y0) * (1.0 - fx) + sample(x0 + 1, y0) * fx;
                let bottom = sample(x0, y0 + 1) * (1.0 - fx) + sample(x0 + 1, y0 + 1) * fx;
                *px = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
        });
    out
}

/// Ink map (255 = ink) of the page shrunk to `side` pixels.
fn ink_thumb(gray: &Gray, side: usize) -> Gray {
    let s = side as f64 / gray.w.max(gray.h) as f64;
    let w = ((gray.w as f64 * s).round() as usize).max(1);
    let h = ((gray.h as f64 * s).round() as usize).max(1);
    let mut small = resize(gray, w, h);
    small.data.iter_mut().for_each(|v| *v = 255 - *v);
    small
}

fn row_sharpness(ink: &Gray, degrees: f64) -> f64 {
    let turned = rotate(ink, degrees, false);
    let rows: Vec<f64> = turned
        .data
        .chunks(turned.w)
        .map(|row| row.iter().map(|&v| v as u64).sum::<u64>() as f64)
        .collect();
    rows.windows(2).map(|p| (p[1] - p[0]).powi(2)).sum()
}

/// Best angle of `angles` (the first one on ties, like `max`).
fn best(ink: &Gray, angles: &[f64]) -> f64 {
    let scores: Vec<f64> = angles.iter().map(|&a| row_sharpness(ink, a)).collect();
    let mut at = 0;
    for (i, &s) in scores.iter().enumerate() {
        if s > scores[at] {
            at = i;
        }
    }
    angles[at]
}

fn steps(from: f64, to: f64, step: f64) -> Vec<f64> {
    let n = ((to - from) / step + 1e-6).floor() as usize;
    (0..=n).map(|i| from + i as f64 * step).collect()
}

/// Degrees (counter-clockwise) that straighten the text rows: a 2 degree
/// grid on a half-size thumbnail, then the two best angles and 0 refined in
/// 0.5 and 0.1 degree steps on an 800 px thumbnail.
pub fn estimate_skew(gray: &Gray) -> f64 {
    const SIDE: usize = 800;
    let fine = ink_thumb(gray, SIDE);
    let ink: u64 = fine.data.iter().map(|&v| v as u64).sum();
    if (ink as f64) < 0.003 * 255.0 * fine.data.len() as f64 {
        return 0.0; // a blank page has no rows to straighten
    }
    let coarse = ink_thumb(gray, SIDE / 2);
    let grid = steps(-DESKEW_MAX, DESKEW_MAX, 2.0);
    let scores: Vec<f64> = grid.iter().map(|&a| row_sharpness(&coarse, a)).collect();
    let mut ranked: Vec<usize> = (0..grid.len()).collect();
    ranked.sort_by(|&a, &b| scores[a].total_cmp(&scores[b]));
    let mut starts = vec![0.0];
    starts.extend(ranked.iter().rev().take(2).map(|&i| grid[i]));
    let mut near: Vec<f64> = Vec::new();
    for a0 in starts {
        for a in steps(a0 - 2.0, a0 + 2.0, 0.5) {
            if !near.iter().any(|&n| (n - a).abs() < 1e-9) {
                near.push(a);
            }
        }
    }
    let a1 = best(&fine, &near);
    best(&fine, &steps(a1 - 0.5, a1 + 0.5, 0.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_by_zero_is_identity_and_quarter_turns_move_pixels() {
        let mut g = Gray::new(5, 5, 255);
        g.data[2 * 5 + 4] = 0; // right of centre
        assert_eq!(rotate(&g, 0.0, true), g);
        // 90 degrees counter-clockwise: the right of centre goes up.
        let r = rotate(&g, 90.0, true);
        let darkest = (0..25).min_by_key(|&i| r.data[i]).unwrap();
        assert_eq!((darkest % 5, darkest / 5), (2, 0), "{:?}", r.data);
    }

    #[test]
    fn a_tilted_page_is_measured_and_a_blank_page_is_left_alone() {
        // Rows of text-like strokes, then tilted by 7 degrees.
        let mut page = Gray::new(400, 300, 255);
        for row in (40..260).step_by(24) {
            for x in (40..360).filter(|x| x % 5 != 0) {
                for y in row..row + 8 {
                    page.data[y * 400 + x] = 0;
                }
            }
        }
        let tilted = rotate(&page, 7.0, true);
        let a = estimate_skew(&tilted);
        assert!((a + 7.0).abs() <= 0.3, "measured {a}");
        assert_eq!(estimate_skew(&Gray::new(100, 100, 255)), 0.0);
    }
}
