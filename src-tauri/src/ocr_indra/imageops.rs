//! Image geometry for the Indra OCR port: the numpy/OpenCV pre- and
//! post-processing of `indra_ocr` (det_post.py, rec_preproc.py, pipeline.py,
//! order_post.py) on plain grayscale buffers. No ONNX Runtime here, so all of
//! it is unit-tested without the models.

/// 8-bit grayscale image, row-major (0 = black, 255 = white).
#[derive(Debug, Clone, PartialEq)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

impl Gray {
    pub fn new(w: usize, h: usize, fill: u8) -> Self {
        Self {
            w,
            h,
            data: vec![fill; w * h],
        }
    }

    /// Grayscale with OpenCV's BT.601 weights, transparency composited over
    /// white (a transparent screenshot would otherwise read as a black page).
    pub fn from_image(image: &image::DynamicImage) -> Self {
        let rgba = image.to_rgba8();
        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
        let data = rgba
            .pixels()
            .map(|p| {
                let [r, g, b, a] = p.0.map(u32::from);
                let over = |c: u32| (c * a + 255 * (255 - a) + 127) / 255;
                ((over(r) * 4899 + over(g) * 9617 + over(b) * 1868 + 8192) >> 14) as u8
            })
            .collect();
        Self { w, h, data }
    }

    fn at(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.w + x]
    }

    /// A quarter turn counter-clockwise (`np.rot90(a, 1)`).
    pub fn rot90_ccw(&self) -> Self {
        let mut out = Self::new(self.h, self.w, 0);
        for y in 0..out.h {
            for x in 0..out.w {
                out.data[y * out.w + x] = self.at(self.w - 1 - y, x);
            }
        }
        out
    }

    /// A quarter turn clockwise (`np.rot90(a, -1)`).
    pub fn rot90_cw(&self) -> Self {
        let mut out = Self::new(self.h, self.w, 0);
        for y in 0..out.h {
            for x in 0..out.w {
                out.data[y * out.w + x] = self.at(y, self.h - 1 - x);
            }
        }
        out
    }
}

/// Source weights of one output sample along an axis.
type Taps = Vec<Vec<(usize, f32)>>;

/// OpenCV `INTER_AREA` for shrinking: each output pixel averages the input
/// span it covers, partial pixels weighted by coverage.
fn area_taps(src: usize, dst: usize) -> Taps {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let (start, end) = (i as f64 * scale, (i + 1) as f64 * scale);
            let mut taps = Vec::new();
            let mut j = start.floor() as usize;
            while (j as f64) < end && j < src {
                let cover = end.min(j as f64 + 1.0) - start.max(j as f64);
                if cover > 1e-9 {
                    taps.push((j, (cover / scale) as f32));
                }
                j += 1;
            }
            taps
        })
        .collect()
}

/// OpenCV `INTER_LINEAR`: half-pixel centres, edges clamped.
fn linear_taps(src: usize, dst: usize) -> Taps {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|i| {
            let fx = ((i as f64 + 0.5) * scale - 0.5).max(0.0);
            let j0 = (fx.floor() as usize).min(src - 1);
            let j1 = (j0 + 1).min(src - 1);
            let t = (fx - j0 as f64).clamp(0.0, 1.0) as f32;
            vec![(j0, 1.0 - t), (j1, t)]
        })
        .collect()
}

/// Resizes to `w` x `h`: area averaging when shrinking, bilinear otherwise
/// (the choice every resize in `indra_ocr` makes).
pub fn resize(src: &Gray, w: usize, h: usize) -> Gray {
    let w = w.max(1);
    let h = h.max(1);
    if (w, h) == (src.w, src.h) {
        return src.clone();
    }
    let shrink = h < src.h;
    let taps = |s, d| {
        if shrink {
            area_taps(s, d)
        } else {
            linear_taps(s, d)
        }
    };
    let (xt, yt) = (taps(src.w, w), taps(src.h, h));
    // Horizontal pass into f32 rows, then vertical.
    let mut rows = vec![0f32; src.h * w];
    for y in 0..src.h {
        let line = &src.data[y * src.w..(y + 1) * src.w];
        for (x, tap) in xt.iter().enumerate() {
            rows[y * w + x] = tap.iter().map(|&(j, k)| line[j] as f32 * k).sum();
        }
    }
    let mut out = Gray::new(w, h, 0);
    for (y, tap) in yt.iter().enumerate() {
        for x in 0..w {
            let v: f32 = tap.iter().map(|&(j, k)| rows[j * w + x] * k).sum();
            out.data[y * w + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The page shrunk to at most `side` pixels a side (`pipeline.preview`).
pub fn preview(gray: &Gray, side: usize) -> Gray {
    let s = side as f64 / gray.w.max(gray.h) as f64;
    if s >= 1.0 {
        return gray.clone();
    }
    resize(gray, scaled(gray.w, s), scaled(gray.h, s))
}

fn scaled(n: usize, s: f64) -> usize {
    ((n as f64 * s).round() as usize).max(1)
}

/// Ink map (1 = ink) padded with paper to `pw` x `ph`.
fn ink_padded(gray: &Gray, pw: usize, ph: usize) -> Vec<f32> {
    let mut out = vec![0f32; pw * ph];
    for y in 0..gray.h.min(ph) {
        for x in 0..gray.w.min(pw) {
            out[y * pw + x] = 1.0 - gray.at(x, y) as f32 / 255.0;
        }
    }
    out
}

/// Detector input (`det_post.page_to_input`): long side scaled to
/// `long_side`, padded to a multiple of 32. Returns (ink, height, width, scale).
pub fn page_to_input(gray: &Gray, long_side: usize) -> (Vec<f32>, usize, usize, f64) {
    let scale = long_side as f64 / gray.w.max(gray.h) as f64;
    let small = resize(gray, scaled(gray.w, scale), scaled(gray.h, scale));
    let ph = small.h.div_ceil(32) * 32;
    let pw = small.w.div_ceil(32) * 32;
    (ink_padded(&small, pw, ph), ph, pw, scale)
}

/// Orientation model input: long side `side`, padded square.
pub fn orient_input(gray: &Gray, side: usize) -> Vec<f32> {
    let s = side as f64 / gray.w.max(gray.h) as f64;
    let small = resize(gray, scaled(gray.w, s), scaled(gray.h, s));
    ink_padded(&small, side, side)
}

/// A detected text line in page pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl LineBox {
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

const OUT_STRIDE: f32 = 2.0;
const CORE_X_SHRINK: f32 = 0.15;

/// Decodes the detector output (`det_post.decode`): `out` is [C, h, w] with
/// core logit, distance to the line top, distance to the line bottom.
/// Boxes are in detector input pixels.
pub fn decode_lines(out: &[f32], h: usize, w: usize) -> Vec<LineBox> {
    let n = h * w;
    if out.len() < 3 * n {
        return Vec::new();
    }
    let mask: Vec<bool> = out[..n].iter().map(|&l| l > 0.0).collect(); // p > 0.5
    let mut label = vec![0u32; n];
    let mut boxes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        let id = boxes.len() as u32 + 1;
        label[start] = id;
        stack.push(start);
        let (mut count, mut top, mut bot) = (0usize, 0f64, 0f64);
        let (mut left, mut right) = (w, 0usize);
        while let Some(i) = stack.pop() {
            let (y, x) = (i / w, i % w);
            let cy = (y as f32 + 0.5) * OUT_STRIDE;
            count += 1;
            top += (cy - out[n + i]) as f64;
            bot += (cy + out[2 * n + i]) as f64;
            left = left.min(x);
            right = right.max(x);
            let mut visit = |j: usize| {
                if mask[j] && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        boxes.push((count, top / count as f64, bot / count as f64, left, right));
    }
    boxes
        .into_iter()
        .filter(|b| b.0 >= 2)
        .map(|(_, y0, y1, left, right)| {
            let (y0, y1) = (y0 as f32, y1 as f32);
            let ex = CORE_X_SHRINK * (y1 - y0).max(1.0);
            LineBox {
                x0: left as f32 * OUT_STRIDE - ex,
                y0,
                x1: (right + 1) as f32 * OUT_STRIDE + ex,
                y1,
            }
        })
        .collect()
}

/// `np.percentile` (linear) of the 2nd, 50th and 98th percentile, from a
/// histogram.
fn percentiles(values: impl Iterator<Item = u8>) -> Option<[f32; 3]> {
    let mut hist = [0usize; 256];
    let mut n = 0usize;
    for v in values {
        hist[v as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let nth = |k: usize| {
        let mut seen = 0;
        for (v, &c) in hist.iter().enumerate() {
            seen += c;
            if seen > k {
                return v as f32;
            }
        }
        255.0
    };
    Some([0.02f64, 0.5, 0.98].map(|q| {
        let x = q * (n - 1) as f64;
        let i = x.floor() as usize;
        let (lo, hi) = (nth(i), nth((i + 1).min(n - 1)));
        lo + (hi - lo) * (x - i as f64) as f32
    }))
}

/// Light text on a dark ground (`rec_preproc.is_dark_line`): read inverted.
pub fn is_dark_line(gray: &Gray, b: &LineBox) -> bool {
    let (x0, x1) = (b.x0.max(0.0) as usize, ((b.x1 as usize) + 1).min(gray.w));
    let (y0, y1) = (b.y0.max(0.0) as usize, ((b.y1 as usize) + 1).min(gray.h));
    if x0 >= x1 || y0 >= y1 {
        return false;
    }
    let region = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y)));
    match percentiles(region.map(|(x, y)| gray.at(x, y))) {
        Some([lo, med, hi]) => hi - med > 2.0 * (med - lo) + 8.0,
        None => false,
    }
}

const PAD_Y: f32 = 0.2;
const PAD_X: f32 = 0.3;
const WIDTH_MULTIPLE: usize = 8;

/// Recognizer input of one line: an ink map `height * window` pixels tall.
pub struct Crop {
    pub h: usize,
    pub w: usize,
    pub ink: Vec<f32>,
}

/// One line cropped and scaled (`rec_preproc.crop_line`). A tall window
/// (`window` > 1) keeps the text scale and adds the page above and below.
pub fn crop_line(gray: &Gray, b: &LineBox, height: usize, window: usize, dark: bool) -> Crop {
    let pad_y = (window as f32 * (1.0 + 2.0 * PAD_Y) - 1.0) / 2.0;
    let height = height * window;
    let h = b.height().max(1.0);
    let ix0 = (b.x0 - PAD_X * h).floor() as i64;
    let iy0 = (b.y0 - pad_y * h).floor() as i64;
    let ix1 = (b.x1 + PAD_X * h).ceil() as i64;
    let iy1 = (b.y1 + pad_y * h).ceil() as i64;
    let cw = (ix1 - ix0).max(1) as usize;
    let ch = (iy1 - iy0).max(1) as usize;
    let mut crop = Gray::new(cw, ch, if dark { 0 } else { 255 });
    for y in iy0.max(0)..iy1.min(gray.h as i64) {
        for x in ix0.max(0)..ix1.min(gray.w as i64) {
            crop.data[(y - iy0) as usize * cw + (x - ix0) as usize] =
                gray.at(x as usize, y as usize);
        }
    }
    let out_w =
        ((cw as f64 * height as f64 / ch as f64).round() as usize).clamp(WIDTH_MULTIPLE, 4096);
    let resized = resize(&crop, out_w, height);
    let ink = resized
        .data
        .iter()
        .map(|&v| {
            let v = v as f32 / 255.0;
            if dark { v } else { 1.0 - v }
        })
        .collect();
    Crop {
        h: height,
        w: out_w,
        ink,
    }
}

/// Right-pads crops of one height to a common width (`pad_batch`): returns
/// the [B, 1, H, W] batch, W, and each crop's width.
pub fn pad_batch(crops: &[&Crop]) -> (Vec<f32>, usize, Vec<i64>) {
    let h = crops[0].h;
    let wm = crops
        .iter()
        .map(|c| c.w)
        .max()
        .unwrap_or(0)
        .div_ceil(WIDTH_MULTIPLE)
        * WIDTH_MULTIPLE;
    let mut out = vec![0f32; crops.len() * h * wm];
    for (k, c) in crops.iter().enumerate() {
        for y in 0..h {
            let dst = k * h * wm + y * wm;
            out[dst..dst + c.w].copy_from_slice(&c.ink[y * c.w..(y + 1) * c.w]);
        }
    }
    (out, wm, crops.iter().map(|c| c.w as i64).collect())
}

/// Groups line indices into batches of similar width (`width_batches`), so
/// little compute goes to padding and no batch grows past 64 x 640 columns.
pub fn width_batches(widths: &[usize]) -> Vec<Vec<usize>> {
    const MAX_BATCH: usize = 64;
    const MAX_RATIO: f32 = 1.3;
    const MIN_WIDTH: usize = 64;
    const MAX_AREA: usize = 64 * 640;
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|&i| widths[i]);
    let mut batches: Vec<Vec<usize>> = Vec::new();
    for i in order {
        let w = widths[i].max(MIN_WIDTH);
        let fits = batches.last().is_some_and(|b| {
            b.len() < MAX_BATCH
                && w as f32 <= MAX_RATIO * widths[b[0]].max(MIN_WIDTH) as f32
                && (b.len() + 1) * w <= MAX_AREA
        });
        if fits {
            batches.last_mut().unwrap().push(i);
        } else {
            batches.push(vec![i]);
        }
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: usize, h: usize, data: &[u8]) -> Gray {
        Gray {
            w,
            h,
            data: data.to_vec(),
        }
    }

    #[test]
    fn quarter_turns_match_numpy_rot90() {
        // [[1, 2, 3], [4, 5, 6]]
        let g = gray(3, 2, &[1, 2, 3, 4, 5, 6]);
        // np.rot90(a, 1) = [[3, 6], [2, 5], [1, 4]]
        assert_eq!(g.rot90_ccw(), gray(2, 3, &[3, 6, 2, 5, 1, 4]));
        // np.rot90(a, -1) = [[4, 1], [5, 2], [6, 3]]
        assert_eq!(g.rot90_cw(), gray(2, 3, &[4, 1, 5, 2, 6, 3]));
        assert_eq!(g.rot90_cw().rot90_ccw(), g);
    }

    #[test]
    fn area_resize_averages_and_linear_resize_interpolates() {
        let g = gray(4, 2, &[0, 100, 200, 255, 0, 100, 200, 255]);
        assert_eq!(resize(&g, 2, 1).data, vec![50, 228]);
        let g = gray(2, 1, &[0, 200]);
        // cv2.resize(INTER_LINEAR) of [0, 200] to width 4: [0, 50, 150, 200]
        assert_eq!(
            resize(&g, 4, 2).data,
            vec![0, 50, 150, 200, 0, 50, 150, 200]
        );
    }

    #[test]
    fn detector_output_decodes_to_one_box_per_component() {
        // 2 x 6 map: one 2 x 2 core at columns 1-2, one lone pixel (dropped).
        let (h, w) = (2, 6);
        let mut out = vec![-10f32; 3 * h * w];
        for i in [1, 2, 7, 8] {
            out[i] = 10.0;
        }
        out[5] = 10.0;
        for i in 0..h * w {
            out[h * w + i] = 3.0;
            out[2 * h * w + i] = 3.0;
        }
        let boxes = decode_lines(&out, h, w);
        assert_eq!(boxes.len(), 1);
        let b = boxes[0];
        // rows 0 and 1 have centres 1 and 3: mean top 2 - 3 = -1, bottom 5.
        assert!(
            (b.y0 + 1.0).abs() < 1e-5 && (b.y1 - 5.0).abs() < 1e-5,
            "{b:?}"
        );
        assert!((b.x0 - (2.0 - 0.9)).abs() < 1e-5 && (b.x1 - (6.0 + 0.9)).abs() < 1e-5);
    }

    #[test]
    fn crop_keeps_text_scale_and_inverts_dark_lines() {
        let mut g = Gray::new(100, 40, 255);
        // Text-like: thin strokes on paper, not a solid block.
        for x in (20..80).step_by(4) {
            for y in 15..25 {
                g.data[y * 100 + x] = 0;
            }
        }
        let b = LineBox {
            x0: 20.0,
            y0: 15.0,
            x1: 80.0,
            y1: 25.0,
        };
        assert!(!is_dark_line(&g, &b));
        let c = crop_line(&g, &b, 32, 1, false);
        assert_eq!(c.h, 32);
        // 66 x 14 px crop scaled to 32 high.
        assert_eq!(c.w, 151);
        let centre_row = &c.ink[16 * c.w..17 * c.w];
        assert!(centre_row.iter().any(|&v| v > 0.5), "line centre has ink");
        assert!(c.ink[..c.w].iter().all(|&v| v < 0.1), "top margin is paper");
        let tall = crop_line(&g, &b, 32, 3, false);
        assert_eq!((tall.h, tall.w), (96, c.w));

        let inverted = Gray {
            data: g.data.iter().map(|v| 255 - v).collect(),
            ..g
        };
        assert!(is_dark_line(&inverted, &b));
    }

    #[test]
    fn batches_group_similar_widths() {
        let batches = width_batches(&[100, 300, 110, 120, 40]);
        assert_eq!(batches, vec![vec![4], vec![0, 2, 3], vec![1]]);
        let (arr, wm, widths) = pad_batch(&[
            &Crop {
                h: 1,
                w: 3,
                ink: vec![1.0; 3],
            },
            &Crop {
                h: 1,
                w: 9,
                ink: vec![1.0; 9],
            },
        ]);
        assert_eq!((wm, widths), (16, vec![3, 9]));
        assert_eq!(arr.iter().filter(|&&v| v == 1.0).count(), 12);
    }
}
