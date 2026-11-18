//! Squarified treemap layout: turns sizes into nested rectangles.

use egui::{pos2, Rect};

/// Lay out `sizes` (sorted largest first) inside `area`. Returns one rect per size.
pub fn layout(sizes: &[f64], area: Rect) -> Vec<Rect> {
    let total: f64 = sizes.iter().sum();
    let mut out = Vec::with_capacity(sizes.len());
    if total <= 0.0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return out;
    }
    let scale = (area.width() as f64 * area.height() as f64) / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    let mut rect = area;
    let mut i = 0;
    while i < areas.len() {
        let short = rect.width().min(rect.height()) as f64;
        // Grow the row while the worst aspect ratio keeps improving.
        let mut j = i + 1;
        while j < areas.len() && worst(&areas[i..=j], short) <= worst(&areas[i..j], short) {
            j += 1;
        }
        let row = &areas[i..j];
        let row_sum: f64 = row.iter().sum();
        if rect.width() >= rect.height() {
            let w = (row_sum / rect.height() as f64) as f32;
            let mut y = rect.top();
            for a in row {
                let h = (a / w as f64) as f32;
                out.push(Rect::from_min_max(pos2(rect.left(), y), pos2(rect.left() + w, y + h)));
                y += h;
            }
            rect.min.x += w;
        } else {
            let h = (row_sum / rect.width() as f64) as f32;
            let mut x = rect.left();
            for a in row {
                let w = (a / h as f64) as f32;
                out.push(Rect::from_min_max(pos2(x, rect.top()), pos2(x + w, rect.top() + h)));
                x += w;
            }
            rect.min.y += h;
        }
        i = j;
    }
    out
}

fn worst(row: &[f64], short: f64) -> f64 {
    let sum: f64 = row.iter().sum();
    let max = row.iter().cloned().fold(0.0, f64::max);
    let min = row.iter().cloned().fold(f64::MAX, f64::min);
    let s2 = short * short;
    let sum2 = sum * sum;
    (s2 * max / sum2).max(sum2 / (s2 * min))
}
