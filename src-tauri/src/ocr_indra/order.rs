//! Reading order of detected lines (`order_post.order_xycut`).

use super::imageops::LineBox;

/// Reading order by recursive XY-cut (`order_post.order_xycut`): split at the
/// widest whitespace gap on either axis, upper or left part first.
pub fn reading_order(boxes: &[LineBox]) -> Vec<usize> {
    fn best_cut(boxes: &[LineBox], ids: &[usize], rows: bool) -> (f32, usize, Vec<usize>) {
        let span = |i: usize| {
            let b = &boxes[i];
            if rows { (b.y0, b.y1) } else { (b.x0, b.x1) }
        };
        let mut order = ids.to_vec();
        order.sort_by(|&a, &b| span(a).0.total_cmp(&span(b).0));
        let mut end = span(order[0]).1;
        let (mut gap, mut at) = (0.0f32, 0usize);
        for (k, &i) in order.iter().enumerate().skip(1) {
            let g = span(i).0 - end;
            if g > gap {
                (gap, at) = (g, k);
            }
            end = end.max(span(i).1);
        }
        (gap, at, order)
    }
    fn top_down(boxes: &[LineBox], ids: &[usize]) -> Vec<usize> {
        let mut heights: Vec<f32> = ids.iter().map(|&i| boxes[i].height()).collect();
        heights.sort_by(f32::total_cmp);
        let half = (heights[heights.len() / 2] * 0.5).max(1e-3);
        let mut out = ids.to_vec();
        out.sort_by(|&a, &b| {
            let row = |i: usize| ((boxes[i].y0 + boxes[i].y1) / 2.0 / half).round();
            row(a)
                .total_cmp(&row(b))
                .then(boxes[a].x0.total_cmp(&boxes[b].x0))
        });
        out
    }
    fn rec(boxes: &[LineBox], ids: Vec<usize>, out: &mut Vec<usize>) {
        if ids.len() <= 1 {
            out.extend(ids);
            return;
        }
        let (gy, ky, oy) = best_cut(boxes, &ids, true);
        let (gx, kx, ox) = best_cut(boxes, &ids, false);
        if ky == 0 && kx == 0 {
            out.extend(top_down(boxes, &ids));
            return;
        }
        let (k, order) = if gy >= gx { (ky, oy) } else { (kx, ox) };
        rec(boxes, order[..k].to_vec(), out);
        rec(boxes, order[k..].to_vec(), out);
    }
    let mut out = Vec::with_capacity(boxes.len());
    rec(boxes, (0..boxes.len()).collect(), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_columns_read_column_by_column() {
        let line = |x0: f32, y0: f32| LineBox {
            x0,
            y0,
            x1: x0 + 80.0,
            y1: y0 + 10.0,
        };
        // Left column rows 0 and 1, right column rows 0 and 1.
        let boxes = [
            line(0.0, 0.0),
            line(120.0, 0.0),
            line(0.0, 15.0),
            line(120.0, 15.0),
        ];
        assert_eq!(reading_order(&boxes), vec![0, 2, 1, 3]);
    }
}
