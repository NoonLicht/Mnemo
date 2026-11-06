//! The Indra OCR pipeline on ONNX Runtime (`pipeline.OrtOCR`), reduced to
//! what search needs: the page turned upright and
//! straightened, text lines detected, each line routed to its script's
//! recognizer and greedily CTC-decoded, lines in reading order. Layout,
//! markdown, math and the n-gram language model of the Python package are
//! left out.

use super::imageops::{
    Crop, Gray, LineBox, crop_line, decode_lines, is_dark_line, orient_input, pad_batch,
    page_to_input, preview, width_batches,
};
use super::order::reading_order;
use super::skew::{DESKEW_MIN, estimate_skew, rotate};
use super::text::{Vocab, flip, is_leader_line, page_is_rtl};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

/// Second detection pass when lines are this small (input pixels), scaled so
/// the median line becomes ADAPTIVE_TARGET_H.
const ADAPTIVE_MIN_H: f32 = 9.0;
const ADAPTIVE_TARGET_H: f32 = 12.0;
const ADAPTIVE_MAX_SIDE: usize = 2048;
const PREVIEW_SIDE: usize = 1600;
const ORIENT_SIDE: usize = 768;
/// A turn is applied only above this probability: a wrong turn loses the page.
const ORIENT_MIN: f32 = 0.95;
/// orient.onnx class: the page is set in vertical columns (Chinese, Japanese).
const VERTICAL: usize = 4;
/// A line under this mean CTC peak probability gets no text (rules,
/// checkboxes and empty fields read as "-", "I", "1").
const MIN_CONF: f32 = 0.5;
/// The script classifier routes a line to one recognizer above this
/// probability; below it the base recognizer reads it too and the more
/// confident reading wins.
const ROUTE_SURE: f32 = 0.95;

struct Group {
    name: String,
    session: Session,
    vocab: Vocab,
}

pub struct Engine {
    det: Session,
    rec: Session,
    vocab: Vocab,
    groups: Vec<Group>,
    script: Option<Session>,
    script_groups: Vec<String>,
    orient: Option<Session>,
    long_side: usize,
    height: usize,
    window: usize,
}

fn session(
    env: &ort::environment::Environment,
    path: &Path,
    threads: usize,
) -> Result<Session, String> {
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    Session::builder(env)
        .map_err(|e| fail(&e))?
        .with_intra_threads(threads)
        .map_err(|e| fail(&e))?
        .with_inter_threads(1)
        .map_err(|e| fail(&e))?
        // Line batches change shape on every call; the arena only grows.
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|e| fail(&e))?
        .commit_from_file(path)
        .map_err(|e| fail(&e))
}

/// One reading of a line: text, mean peak probability.
type Reading = (String, f32);

impl Engine {
    pub fn load(
        env: &ort::environment::Environment,
        dir: &Path,
        threads: usize,
    ) -> Result<Self, String> {
        let meta: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("meta.json")).map_err(|e| format!("meta.json: {e}"))?,
        )
        .map_err(|e| format!("meta.json: {e}"))?;
        let num = |key: &str, default: u64| {
            meta.get(key).and_then(|v| v.as_u64()).unwrap_or(default) as usize
        };
        let script_groups: Vec<String> = meta
            .get("script_groups")
            .and_then(|g| serde_json::from_value(g.clone()).ok())
            .unwrap_or_else(|| ["base", "cjk", "rtl", "other"].map(String::from).to_vec());
        let mut groups = Vec::new();
        for name in &script_groups {
            let path = dir.join(format!("rec_{name}.onnx"));
            if let Some(entry) = meta.get(format!("vocab_{name}"))
                && path.is_file()
            {
                groups.push(Group {
                    name: name.clone(),
                    session: session(env, &path, threads)?,
                    vocab: Vocab::from_meta(entry)?,
                });
            }
        }
        let optional = |name: &str| {
            let path = dir.join(name);
            path.is_file()
                .then(|| session(env, &path, threads))
                .transpose()
        };
        Ok(Self {
            det: session(env, &dir.join("det.onnx"), threads)?,
            rec: session(env, &dir.join("rec.onnx"), threads)?,
            vocab: Vocab::from_meta(meta.get("vocab").ok_or("meta.json: no vocab")?)?,
            script: if groups.is_empty() {
                None
            } else {
                optional("script.onnx")?
            },
            orient: optional("orient.onnx")?,
            groups,
            script_groups,
            long_side: num("det_long_side", 1024),
            height: num("rec_height", 32),
            window: num("rec_window", 1).max(1),
        })
    }

    /// The page's text, one detected line per line, in reading order.
    pub fn read(&mut self, page: &Gray) -> Result<String, String> {
        if page.w < 2 || page.h < 2 {
            return Ok(String::new());
        }
        // The orientation model reads the preview straightened by its row
        // angle (a tilted page could read as turned).
        let small = preview(page, PREVIEW_SIDE);
        let mut skew = estimate_skew(&small);
        let turn = if skew.abs() >= DESKEW_MIN {
            self.orientation(&rotate(&small, skew, true))?
        } else {
            self.orientation(&small)?
        };
        let quarter = |g: &Gray| match turn {
            VERTICAL => g.rot90_ccw(),
            k => (0..k).fold(g.clone(), |g, _| g.rot90_cw()),
        };
        let mut upright = quarter(page);
        // After a quarter turn the rows were columns: measure again.
        if turn % 2 == 1 || turn == VERTICAL {
            skew = estimate_skew(&quarter(&small));
        }
        if skew.abs() >= DESKEW_MIN {
            upright = rotate(&upright, skew, true);
        }
        let boxes = self.detect(&upright)?;
        if boxes.is_empty() {
            return Ok(String::new());
        }
        let readings = self.recognize(&upright, &boxes, turn == VERTICAL)?;
        let rtl = page_is_rtl(readings.iter().map(|r| r.0.as_str()));
        let mut kept = Vec::new();
        let mut texts = Vec::new();
        for (b, (text, _)) in boxes.iter().zip(readings) {
            let text = flip(&text, rtl);
            if !text.trim().is_empty() && !is_leader_line(&text, b.x1 - b.x0, b.height()) {
                kept.push(*b);
                texts.push(text);
            }
        }
        // A right-to-left page reads its right column first: order mirrored.
        if rtl {
            let w = upright.w as f32;
            for b in &mut kept {
                (b.x0, b.x1) = (w - b.x1, w - b.x0);
            }
        }
        let order = reading_order(&kept);
        Ok(order
            .iter()
            .map(|&i| texts[i].as_str())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Quarter turns clockwise that make the page upright, or VERTICAL, from
    /// the page preview.
    fn orientation(&mut self, page: &Gray) -> Result<usize, String> {
        let Some(orient) = self.orient.as_mut() else {
            return Ok(0);
        };
        let x = orient_input(page, ORIENT_SIDE);
        let side = ORIENT_SIDE as i64;
        let input = Tensor::from_array(([1i64, 1, side, side], x)).map_err(|e| e.to_string())?;
        let out = orient
            .run(ort::inputs!["x" => input])
            .map_err(|e| e.to_string())?;
        let (_, p) = out["p"]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let (k, best) =
            p.iter().enumerate().fold(
                (0, f32::MIN),
                |acc, (i, &v)| if v > acc.1 { (i, v) } else { acc },
            );
        Ok(if best > ORIENT_MIN { k } else { 0 })
    }

    fn detect_at(&mut self, page: &Gray, long_side: usize) -> Result<(Vec<LineBox>, f64), String> {
        let (x, h, w, scale) = page_to_input(page, long_side);
        let input =
            Tensor::from_array(([1i64, 1, h as i64, w as i64], x)).map_err(|e| e.to_string())?;
        let out = self
            .det
            .run(ort::inputs!["x" => input])
            .map_err(|e| e.to_string())?;
        let (shape, data) = out[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let (oh, ow) = (shape[2] as usize, shape[3] as usize);
        Ok((decode_lines(data, oh, ow), scale))
    }

    fn detect(&mut self, page: &Gray) -> Result<Vec<LineBox>, String> {
        let (mut boxes, mut scale) = self.detect_at(page, self.long_side)?;
        if boxes.len() >= 8 {
            let mut heights: Vec<f32> = boxes.iter().map(LineBox::height).collect();
            heights.sort_by(f32::total_cmp);
            let median = heights[heights.len() / 2];
            if median < ADAPTIVE_MIN_H {
                let side = (self.long_side as f32 * ADAPTIVE_TARGET_H / median.max(1.0)) as usize;
                let side = side.min(ADAPTIVE_MAX_SIDE) / 32 * 32;
                if side > self.long_side {
                    (boxes, scale) = self.detect_at(page, side)?;
                }
            }
        }
        let (w, h, s) = (page.w as f32, page.h as f32, scale as f32);
        for b in &mut boxes {
            b.x0 = (b.x0 / s).clamp(0.0, w);
            b.x1 = (b.x1 / s).clamp(0.0, w);
            b.y0 = (b.y0 / s).clamp(0.0, h);
            b.y1 = (b.y1 / s).clamp(0.0, h);
        }
        Ok(boxes)
    }

    /// Text and confidence per line box (`OrtOCR.recognize`).
    fn recognize(
        &mut self,
        page: &Gray,
        boxes: &[LineBox],
        vertical: bool,
    ) -> Result<Vec<Reading>, String> {
        let dark: Vec<bool> = boxes.iter().map(|b| is_dark_line(page, b)).collect();
        let crops: Vec<Crop> = boxes
            .iter()
            .zip(&dark)
            .map(|(b, &d)| crop_line(page, b, self.height, 1, d))
            .collect();
        let tall: Vec<Crop> = if self.window > 1 {
            boxes
                .iter()
                .zip(&dark)
                .map(|(b, &d)| crop_line(page, b, self.height, self.window, d))
                .collect()
        } else {
            Vec::new()
        };
        let all: Vec<usize> = (0..crops.len()).collect();
        // (group index or None for base, line indices)
        let mut plan: Vec<(Option<usize>, Vec<usize>)> = Vec::new();
        let cjk = self.groups.iter().position(|g| g.name == "cjk");
        if vertical && cjk.is_some() {
            // A vertical page's lines are CJK columns turned on their side.
            plan.push((cjk, all));
        } else if self.script.is_some() {
            let (names, sure) = self.line_groups(&crops)?;
            let base = all
                .iter()
                .copied()
                .filter(|&j| names[j].is_none() || !sure[j])
                .collect();
            plan.push((None, base));
            for g in 0..self.groups.len() {
                plan.push((
                    Some(g),
                    all.iter()
                        .copied()
                        .filter(|&j| names[j] == Some(g))
                        .collect(),
                ));
            }
        } else {
            plan.push((None, all));
        }

        let mut best: Vec<Reading> = vec![(String::new(), -1.0); crops.len()];
        for (group, lines) in plan {
            let src = if group.is_none() && self.window > 1 {
                &tall
            } else {
                &crops
            };
            let widths: Vec<usize> = lines.iter().map(|&j| crops[j].w).collect();
            for part in width_batches(&widths) {
                let idx: Vec<usize> = part.iter().map(|&i| lines[i]).collect();
                let batch: Vec<&Crop> = idx.iter().map(|&j| &src[j]).collect();
                let (session, vocab) = match group {
                    Some(g) => {
                        let Group { session, vocab, .. } = &mut self.groups[g];
                        (session, &*vocab)
                    }
                    None => (&mut self.rec, &self.vocab),
                };
                for (j, (ids, conf)) in idx.iter().zip(run_ctc(session, &batch)?) {
                    if conf <= best[*j].1 {
                        continue;
                    }
                    let text = if conf < MIN_CONF {
                        String::new()
                    } else if group.is_some() {
                        vocab.decode_chars(&ids)
                    } else {
                        vocab.decode(&ids)
                    };
                    best[*j] = (text, conf);
                }
            }
        }

        // Fallback for lines that yielded no text from specialized recognizers:
        // try the comprehensive base recognizer (`rec.onnx` + `vocab`) which includes
        // full Cyrillic, Latin, Greek, math, and punctuation. This ensures misrouted
        // or out-of-vocabulary scripts are never silently dropped.
        let empty_lines: Vec<usize> = (0..crops.len())
            .filter(|&j| best[j].0.trim().is_empty())
            .collect();
        if !empty_lines.is_empty() {
            let src = if self.window > 1 { &tall } else { &crops };
            let widths: Vec<usize> = empty_lines.iter().map(|&j| crops[j].w).collect();
            for part in width_batches(&widths) {
                let idx: Vec<usize> = part.iter().map(|&i| empty_lines[i]).collect();
                let batch: Vec<&Crop> = idx.iter().map(|&j| &src[j]).collect();
                for (j, (ids, conf)) in idx.iter().zip(run_ctc(&mut self.rec, &batch)?) {
                    if conf >= MIN_CONF {
                        let text = self.vocab.decode(&ids);
                        if !text.trim().is_empty() && conf > best[*j].1 {
                            best[*j] = (text, conf);
                        }
                    }
                }
            }
        }

        Ok(best)
    }

    /// Per line: the group whose recognizer reads it (None = base) and
    /// whether the classifier is sure.
    fn line_groups(&mut self, crops: &[Crop]) -> Result<(Vec<Option<usize>>, Vec<bool>), String> {
        let mut names = vec![None; crops.len()];
        let mut sure = vec![true; crops.len()];
        let Some(script) = self.script.as_mut() else {
            return Ok((names, sure));
        };
        let widths: Vec<usize> = crops.iter().map(|c| c.w).collect();
        for idx in width_batches(&widths) {
            let batch: Vec<&Crop> = idx.iter().map(|&j| &crops[j]).collect();
            let out = script
                .run(ort::inputs![
                    "x" => batch_tensor(&batch)?,
                    "widths" => widths_tensor(&batch)?,
                ])
                .map_err(|e| e.to_string())?;
            let (shape, p) = out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| e.to_string())?;
            let classes = shape.get(1).copied().unwrap_or(1).max(1) as usize;
            for (k, &j) in idx.iter().enumerate() {
                let row = &p[k * classes..(k + 1) * classes];
                let (arg, max) = row.iter().enumerate().fold((0, f32::MIN), |acc, (i, &v)| {
                    if v > acc.1 { (i, v) } else { acc }
                });
                let name = self
                    .script_groups
                    .get(arg)
                    .map(String::as_str)
                    .unwrap_or("base");
                names[j] = self.groups.iter().position(|g| g.name == name);
                sure[j] = max > ROUTE_SURE;
            }
        }
        Ok((names, sure))
    }
}

fn batch_tensor(batch: &[&Crop]) -> Result<Tensor<f32>, String> {
    let (data, w, _) = pad_batch(batch);
    let shape = [batch.len() as i64, 1, batch[0].h as i64, w as i64];
    Tensor::from_array((shape, data)).map_err(|e| e.to_string())
}

fn widths_tensor(batch: &[&Crop]) -> Result<Tensor<i64>, String> {
    let widths: Vec<i64> = batch.iter().map(|c| c.w as i64).collect();
    Tensor::from_array(([batch.len() as i64], widths)).map_err(|e| e.to_string())
}

/// Runs a CTC recognizer on a batch and greedily decodes each line: repeats
/// and blanks dropped, confidence = mean peak probability of what is kept.
fn run_ctc(session: &mut Session, batch: &[&Crop]) -> Result<Vec<(Vec<i64>, f32)>, String> {
    let out = session
        .run(ort::inputs![
            "x" => batch_tensor(batch)?,
            "widths" => widths_tensor(batch)?,
        ])
        .map_err(|e| e.to_string())?;
    let (shape, ids) = out["ids"]
        .try_extract_tensor::<i64>()
        .map_err(|e| e.to_string())?;
    let (_, conf) = out["conf"]
        .try_extract_tensor::<f32>()
        .map_err(|e| e.to_string())?;
    let (_, lengths) = out["lengths"]
        .try_extract_tensor::<i64>()
        .map_err(|e| e.to_string())?;
    let steps = shape.get(1).copied().unwrap_or(0).max(0) as usize;
    Ok((0..batch.len())
        .map(|k| {
            let n = (lengths.get(k).copied().unwrap_or(0).max(0) as usize).min(steps);
            let seq = &ids[k * steps..k * steps + n];
            let pm = &conf[k * steps..k * steps + n];
            let (mut kept, mut sum) = (Vec::new(), 0f32);
            for t in 0..n {
                if seq[t] != 0 && (t == 0 || seq[t] != seq[t - 1]) {
                    kept.push(seq[t]);
                    sum += pm[t];
                }
            }
            let mean = if kept.is_empty() {
                0.0
            } else {
                sum / kept.len() as f32
            };
            (kept, mean)
        })
        .collect())
}
