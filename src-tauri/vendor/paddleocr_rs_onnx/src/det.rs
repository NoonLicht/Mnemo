//! Text detection module using DBNet.
//!
//! Detects text regions in images by producing probability maps and finding
//! contours. The pipeline is:
//!
//! 1. **Preprocess** — Resize, pad to multiple of 32, normalize with ImageNet mean/std
//! 2. **Inference** — Run DBNet to produce a per-pixel probability map
//! 3. **Postprocess** — Threshold, find contours, compute minimum rotated boxes,
//!    filter by size and confidence, expand with unclip ratio

use crate::error::PaddleOcrError;

use geo::algorithm::buffer::BufferStyle;
use geo::{Area, Buffer, LineString as GeoLineString, MinimumRotatedRect, MultiPolygon, Polygon as GeoPolygon};
use image::{DynamicImage, GrayImage, Luma};
use imageproc::contours::find_contours;
use imageproc::drawing::draw_polygon_mut;
use imageproc::point::Point;
use ort::session::Session;
use crate::TextRegion;

const DET_LONG_SIDE_DEFAULT: u32 = 960;

/// Longest side the detector sees. `MNEMO_DET_LONG_SIDE` overrides it (Mnemo patch),
/// so the speed/recall trade-off can be measured.
fn det_canvas() -> Option<u32> {
    static VALUE: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("MNEMO_DET_CANVAS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| (64..=4096).contains(v) && v % 32 == 0)
    })
}

fn det_long_side() -> u32 {
    static VALUE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *VALUE.get_or_init(|| {
        std::env::var("MNEMO_DET_LONG_SIDE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| (160..=4096).contains(v))
            .unwrap_or(DET_LONG_SIDE_DEFAULT)
    })
}
const DET_THRESHOLD: f32 = 0.3;
const BOX_THRESHOLD: f32 = 0.6;
const UNCLIP_RATIO: f32 = 2.0;
const MAX_CANDIDATES: usize = 1000;
const MIN_SIZE: f32 = 3.0;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

fn preprocess(image: &DynamicImage) -> Result<(Vec<f32>, i64, i64, f32, f32), PaddleOcrError> {
    let (w, h) = (image.width(), image.height());
    let scale = det_long_side() as f32 / w.max(h) as f32;
    let scale = scale.min(1.0);
    let new_w = (w as f32 * scale) as u32;
    let new_h = (h as f32 * scale) as u32;

    let resized = image.resize_exact(new_w, new_h, image::imageops::FilterType::Triangle);
    let pad_w = (32 - new_w % 32) % 32;
    let pad_h = (32 - new_h % 32) % 32;
    // `MNEMO_DET_CANVAS`: pad every input to one fixed square so a GPU sees a single
    // input shape instead of compiling kernels for each screenshot size.
    let (padded_w, padded_h) = match det_canvas() {
        Some(c) => ((new_w + pad_w).max(c), (new_h + pad_h).max(c)),
        None => (new_w + pad_w, new_h + pad_h),
    };

    let rgb = resized.to_rgb8();
    let (pw, ph) = (padded_w as usize, padded_h as usize);
    let plane = pw * ph;
    // Padding is black, i.e. (0 - mean) / std per channel.
    let mut data = vec![0.0f32; 3 * plane];
    let luts: [[f32; 256]; 3] = std::array::from_fn(|c| {
        std::array::from_fn(|v| (v as f32 / 255.0 - MEAN[c]) / STD[c])
    });
    for c in 0..3 {
        data[c * plane..(c + 1) * plane].fill(luts[c][0]);
    }
    for (y, row) in rgb.rows().enumerate() {
        for (x, px) in row.enumerate() {
            let o = y * pw + x;
            data[o] = luts[0][px[0] as usize];
            data[plane + o] = luts[1][px[1] as usize];
            data[2 * plane + o] = luts[2][px[2] as usize];
        }
    }

    let out_h = padded_h as i64;
    let out_w = padded_w as i64;
    Ok((data, out_h, out_w, scale, scale))
}

fn postprocess(
    prob_data: &[f32],
    out_h: usize,
    out_w: usize,
    orig_w: u32,
    orig_h: u32,
    scale_x: f32,
    scale_y: f32,
) -> Vec<TextRegion> {
    let mut binary = GrayImage::new(out_w as u32, out_h as u32);
    for y in 0..out_h {
        for x in 0..out_w {
            let val = if prob_data[y * out_w + x] > DET_THRESHOLD { 255u8 } else { 0u8 };
            binary.put_pixel(x as u32, y as u32, Luma([val]));
        }
    }

    let contours = find_contours::<i32>(&binary);
    let mut regions = Vec::new();

    for contour in contours.iter().take(MAX_CANDIDATES) {
        if contour.points.len() < 4 {
            continue;
        }

        let contour_points: Vec<[f32; 2]> = contour
            .points
            .iter()
            .map(|p| [p.x as f32, p.y as f32])
            .collect();

        let (mini_box, min_side) = get_mini_box(&contour_points);
        if min_side < MIN_SIZE {
            continue;
        }

        let prob_val = average_probability(prob_data, out_w, out_h, &mini_box);
        if prob_val < BOX_THRESHOLD {
            continue;
        }

        let Some(expanded) = unclip_box(&mini_box, UNCLIP_RATIO) else {
            continue;
        };
        let (expanded_box, min_side) = get_mini_box(&expanded);
        if min_side < MIN_SIZE + 2.0 {
            continue;
        }

        let inv_sx = 1.0 / scale_x;
        let inv_sy = 1.0 / scale_y;
        let bbox = expanded_box.map(|p| {
            let x = (p[0] * inv_sx)
                .round()
                .clamp(0.0, orig_w.saturating_sub(1) as f32);
            let y = (p[1] * inv_sy)
                .round()
                .clamp(0.0, orig_h.saturating_sub(1) as f32);
            [x, y]
        });
        let bbox = sort_box_points_like_paddle(bbox);
        let rect_width = point_dist(bbox[0], bbox[1]);
        let rect_height = point_dist(bbox[0], bbox[3]);
        if rect_width <= MIN_SIZE || rect_height <= MIN_SIZE {
            continue;
        }

        regions.push(TextRegion { bbox, confidence: prob_val });
    }

    regions
}

fn get_mini_box(points: &[[f32; 2]]) -> ([[f32; 2]; 4], f32) {
    let rect = minimum_rotated_box(points).unwrap_or([[0.0, 0.0]; 4]);
    let side1 = point_dist(rect[0], rect[1]);
    let side2 = point_dist(rect[1], rect[2]);
    (rect, side1.min(side2))
}

fn polygon_area(poly: &[[f32; 2]]) -> f32 {
    let mut area = 0.0;
    for i in 0..poly.len() {
        let j = (i + 1) % poly.len();
        area += poly[i][0] * poly[j][1] - poly[j][0] * poly[i][1];
    }
    area.abs() / 2.0
}

fn polygon_perimeter(poly: &[[f32; 2]]) -> f32 {
    let mut perim = 0.0;
    for i in 0..poly.len() {
        let j = (i + 1) % poly.len();
        let dx = poly[i][0] - poly[j][0];
        let dy = poly[i][1] - poly[j][1];
        perim += (dx * dx + dy * dy).sqrt();
    }
    perim
}

fn minimum_rotated_box(points: &[[f32; 2]]) -> Option<[[f32; 2]; 4]> {
    if points.len() < 3 {
        return None;
    }
    let line = GeoLineString::from(
        points
            .iter()
            .map(|p| (p[0] as f64, p[1] as f64))
            .collect::<Vec<_>>(),
    );
    let polygon = line.minimum_rotated_rect()?;
    polygon_to_box(&polygon)
}

fn polygon_to_box(poly: &GeoPolygon<f64>) -> Option<[[f32; 2]; 4]> {
    let coords = &poly.exterior().0;
    if coords.len() < 4 {
        return None;
    }
    let mut rect = [[0.0; 2]; 4];
    for (idx, coord) in coords.iter().take(4).enumerate() {
        rect[idx] = [coord.x as f32, coord.y as f32];
    }
    Some(sort_box_points_like_paddle(rect))
}

fn unclip_box(poly: &[[f32; 2]; 4], ratio: f32) -> Option<Vec<[f32; 2]>> {
    let area = polygon_area(poly);
    let perimeter = polygon_perimeter(poly);
    let distance = area * ratio / perimeter.max(1.0);
    if distance <= 0.0 {
        return None;
    }

    let ring = GeoLineString::from(vec![
        (poly[0][0] as f64, poly[0][1] as f64),
        (poly[1][0] as f64, poly[1][1] as f64),
        (poly[2][0] as f64, poly[2][1] as f64),
        (poly[3][0] as f64, poly[3][1] as f64),
        (poly[0][0] as f64, poly[0][1] as f64),
    ]);
    let polygon = GeoPolygon::new(ring, Vec::new());
    let style = BufferStyle::new(distance as f64);
    let buffered: MultiPolygon<f64> = polygon.buffer_with_style(style);
    let largest = buffered
        .0
        .iter()
        .max_by(|a, b| a.unsigned_area().partial_cmp(&b.unsigned_area()).unwrap())?;
    let rect = largest.minimum_rotated_rect()?;
    let box_points = polygon_to_box(&rect)?;
    Some(box_points.into_iter().collect())
}

fn point_dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    (dx * dx + dy * dy).sqrt()
}

fn sort_box_points_like_paddle(mut pts: [[f32; 2]; 4]) -> [[f32; 2]; 4] {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));

    let (left0, left1) = (pts[0], pts[1]);
    let (right0, right1) = (pts[2], pts[3]);

    let (top_left, bottom_left) = if left1[1] > left0[1] {
        (left0, left1)
    } else {
        (left1, left0)
    };
    let (top_right, bottom_right) = if right1[1] > right0[1] {
        (right0, right1)
    } else {
        (right1, right0)
    };

    [top_left, top_right, bottom_right, bottom_left]
}

fn average_probability(prob_data: &[f32], w: usize, h: usize, poly: &[[f32; 2]; 4]) -> f32 {
    let min_x = poly
        .iter()
        .map(|p| p[0].floor() as isize)
        .min()
        .unwrap_or(0)
        .clamp(0, w as isize - 1) as usize;
    let max_x = poly
        .iter()
        .map(|p| p[0].ceil() as isize)
        .max()
        .unwrap_or(0)
        .clamp(0, w as isize - 1) as usize;
    let min_y = poly
        .iter()
        .map(|p| p[1].floor() as isize)
        .min()
        .unwrap_or(0)
        .clamp(0, h as isize - 1) as usize;
    let max_y = poly
        .iter()
        .map(|p| p[1].ceil() as isize)
        .max()
        .unwrap_or(0)
        .clamp(0, h as isize - 1) as usize;

    let local_poly = poly.map(|p| [p[0] - min_x as f32, p[1] - min_y as f32]);
    let draw_poly = local_poly.map(|p| Point::new(p[0] as i32, p[1] as i32));
    let mut mask = GrayImage::new((max_x - min_x + 1) as u32, (max_y - min_y + 1) as u32);
    draw_polygon_mut(&mut mask, &draw_poly, Luma([1u8]));

    let mut sum = 0.0f64;
    let mut count = 0usize;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if mask.get_pixel((x - min_x) as u32, (y - min_y) as u32)[0] != 0 {
                sum += prob_data[y * w + x] as f64;
                count += 1;
            }
        }
    }

    if count == 0 {
        0.0
    } else {
        (sum / count as f64) as f32
    }
}

/// Detect text regions in an image using the DBNet detection model.
///
/// Runs the full detection pipeline: preprocessing, ONNX inference, and
/// postprocessing (contour finding, minimum rotated box, filtering).
///
/// # Arguments
///
/// * `session` — The ONNX runtime session loaded with the detection model.
/// * `image` — The input image to process.
/// * `input_name` — Name of the model's input tensor.
/// * `output_name` — Name of the model's output tensor.
///
/// # Returns
///
/// A vector of [`TextRegion`] instances, each with a quadrilateral bounding box
/// and detection confidence score.
///
/// # Errors
///
/// Returns [`PaddleOcrError::Preprocessing`] if image preprocessing fails,
/// or [`PaddleOcrError::Inference`] if the model execution fails.
/// Mnemo patch: a preprocessed detector input. Splitting detection into
/// `prepare` / `infer` / `finish` lets callers hold a session only for `infer`.
pub struct DetInput {
    data: Vec<f32>,
    height: i64,
    width: i64,
    scale: f32,
}

impl DetInput {
    pub fn scale(&self) -> f32 {
        self.scale
    }
}

pub fn prepare(image: &DynamicImage) -> Result<DetInput, PaddleOcrError> {
    let t = std::time::Instant::now();
    let (data, height, width, scale, _) = preprocess(image)?;
    crate::stage_stats::add(crate::stage_stats::DET_PRE, t);
    Ok(DetInput { data, height, width, scale })
}

/// Runs the detector and returns the probability map with its size.
pub fn infer(
    session: &mut Session,
    input: DetInput,
    input_name: &str,
    output_name: &str,
) -> Result<(Vec<f32>, usize, usize), PaddleOcrError> {
    let t = std::time::Instant::now();
    let input_tensor =
        ort::value::Tensor::from_array(([1i64, 3, input.height, input.width], input.data))?;
    let outputs = session.run(ort::inputs![input_name => input_tensor])?;
    let (shape, slice) = outputs[output_name].try_extract_tensor::<f32>()?;
    let result = (slice.to_vec(), shape[2] as usize, shape[3] as usize);
    crate::stage_stats::add(crate::stage_stats::DET_RUN, t);
    Ok(result)
}

pub fn finish(
    prob: &[f32],
    out_h: usize,
    out_w: usize,
    orig_w: u32,
    orig_h: u32,
    scale: f32,
) -> Vec<TextRegion> {
    let t = std::time::Instant::now();
    let regions = postprocess(prob, out_h, out_w, orig_w, orig_h, scale, scale);
    crate::stage_stats::add(crate::stage_stats::DET_POST, t);
    regions
}
