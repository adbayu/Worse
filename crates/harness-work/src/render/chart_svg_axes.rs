//! Axes, bar/column, line/area, and scatter SVG chart rendering (companion to `chart_svg.rs`).

use std::fmt::Write as _;

use super::chart_svg::{color, esc, nice_step, short, Style};
use crate::model::chart::{Chart, ChartType};

#[allow(
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]
pub(super) fn axes_chart(
    s: &mut String,
    chart: &Chart,
    (w, top, bottom): (f64, f64, f64),
    st: &Style,
) {
    let scatter = chart.r#type == ChartType::Scatter;
    let horizontal = chart.r#type == ChartType::Bar;
    let (lo, hi) = chart.value_range();
    let step = nice_step(hi - lo);
    let lo = (lo / step).floor() * step;
    let hi = (hi / step).ceil() * step;
    let left = 14.0
        + 7.5 * short(hi).len().max(short(lo).len()) as f64
        + if chart.y_label.is_some() { 16.0 } else { 0.0 };
    let left = if horizontal {
        14.0 + 7.0
            * chart
                .categories
                .iter()
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(4)
                .min(18) as f64
    } else {
        left
    };
    let plot_bottom = bottom - 22.0 - if chart.x_label.is_some() { 16.0 } else { 0.0 };
    let (px, py, pw, ph) = (left, top + 6.0, w - left - 14.0, plot_bottom - top - 6.0);
    let mut v = lo;
    while v <= hi + step / 2.0 {
        let t = (v - lo) / (hi - lo);
        if horizontal {
            let x = px + t * pw;
            let _ = write!(
                s,
                r#"<line x1="{x:.1}" y1="{py}" x2="{x:.1}" y2="{}" stroke="{}"/><text x="{x:.1}" y="{}" text-anchor="middle" fill="{}">{}</text>"#,
                py + ph,
                st.grid,
                py + ph + 16.0,
                st.muted,
                short(v)
            );
        } else {
            let y = py + ph - t * ph;
            let _ = write!(
                s,
                r#"<line x1="{px}" y1="{y:.1}" x2="{}" y2="{y:.1}" stroke="{}"/><text x="{}" y="{:.1}" text-anchor="end" fill="{}">{}</text>"#,
                px + pw,
                st.grid,
                px - 6.0,
                y + 4.0,
                st.muted,
                short(v)
            );
        }
        v += step;
    }
    if let Some(l) = &chart.y_label {
        let _ = write!(
            s,
            r#"<text transform="translate(12,{}) rotate(-90)" text-anchor="middle" fill="{}">{}</text>"#,
            py + ph / 2.0,
            st.muted,
            esc(l)
        );
    }
    if let Some(l) = &chart.x_label {
        let _ = write!(
            s,
            r#"<text x="{}" y="{}" text-anchor="middle" fill="{}">{}</text>"#,
            px + pw / 2.0,
            bottom - 2.0,
            st.muted,
            esc(l)
        );
    }
    let scale = |v: f64| (v - lo) / (hi - lo);
    if scatter {
        scatter_points(s, chart, (px, py, pw, ph), st, &scale);
        return;
    }
    let n = chart.categories.len().max(1);
    let band = if horizontal { ph } else { pw } / n as f64;
    for (i, c) in chart.categories.iter().enumerate() {
        let mid = band * (i as f64 + 0.5);
        let label: String = c.chars().take(18).collect();
        if horizontal {
            let _ = write!(
                s,
                r#"<text x="{}" y="{:.1}" text-anchor="end" fill="{}">{}</text>"#,
                px - 6.0,
                py + mid + 4.0,
                st.muted,
                esc(&label)
            );
        } else if n <= 16 || i % (n / 12 + 1) == 0 {
            let _ = write!(
                s,
                r#"<text x="{:.1}" y="{}" text-anchor="middle" fill="{}">{}</text>"#,
                px + mid,
                py + ph + 16.0,
                st.muted,
                esc(&label)
            );
        }
    }
    let kind_of = |i: usize| match chart.r#type {
        ChartType::Combo => chart.series[i].r#type.unwrap_or(if i == 0 {
            ChartType::Column
        } else {
            ChartType::Line
        }),
        t => t,
    };
    let bars: Vec<usize> = (0..chart.series.len())
        .filter(|i| matches!(kind_of(*i), ChartType::Bar | ChartType::Column))
        .collect();
    let mut stack_pos = vec![0.0; n];
    let mut stack_neg = vec![0.0; n];
    for (k, &si) in bars.iter().enumerate() {
        let c = color(chart, si, st);
        let slots = if chart.stacked {
            1.0
        } else {
            bars.len() as f64
        };
        let bw = band * 0.72 / slots;
        for (i, v) in chart.series[si].values.iter().enumerate().take(n) {
            let base = if chart.stacked {
                if *v >= 0.0 {
                    stack_pos[i]
                } else {
                    stack_neg[i]
                }
            } else {
                0.0
            };
            let (a, b) = (scale(base), scale(base + v));
            if chart.stacked {
                if *v >= 0.0 {
                    stack_pos[i] += v;
                } else {
                    stack_neg[i] += v;
                }
            }
            let off =
                band * i as f64 + band * 0.14 + if chart.stacked { 0.0 } else { bw * k as f64 };
            if horizontal {
                let _ = write!(
                    s,
                    r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{bw:.1}" rx="2" fill="{c}"/>"#,
                    px + a.min(b) * pw,
                    py + off,
                    (b - a).abs() * pw
                );
            } else {
                let _ = write!(
                    s,
                    r#"<rect x="{:.1}" y="{:.1}" width="{bw:.1}" height="{:.1}" rx="2" fill="{c}"/>"#,
                    px + off,
                    py + ph - a.max(b) * ph,
                    (b - a).abs() * ph
                );
            }
        }
    }
    for si in
        (0..chart.series.len()).filter(|i| matches!(kind_of(*i), ChartType::Line | ChartType::Area))
    {
        let c = color(chart, si, st);
        let pts: Vec<(f64, f64)> = chart.series[si]
            .values
            .iter()
            .enumerate()
            .take(n)
            .map(|(i, v)| (px + band * (i as f64 + 0.5), py + ph - scale(*v) * ph))
            .collect();
        let line: Vec<String> = pts.iter().map(|(x, y)| format!("{x:.1},{y:.1}")).collect();
        if kind_of(si) == ChartType::Area {
            if let (Some(f), Some(l)) = (pts.first(), pts.last()) {
                let base = py + ph - scale(0.0) * ph;
                let _ = write!(
                    s,
                    r#"<polygon points="{:.1},{base:.1} {} {:.1},{base:.1}" fill="{c}" fill-opacity="0.25"/>"#,
                    f.0,
                    line.join(" "),
                    l.0
                );
            }
        }
        let _ = write!(
            s,
            r#"<polyline points="{}" fill="none" stroke="{c}" stroke-width="2.5" stroke-linejoin="round"/>"#,
            line.join(" ")
        );
        for (x, y) in pts {
            let _ = write!(s, r#"<circle cx="{x:.1}" cy="{y:.1}" r="3" fill="{c}"/>"#);
        }
    }
}

fn scatter_points(
    s: &mut String,
    chart: &Chart,
    (px, py, pw, ph): (f64, f64, f64, f64),
    st: &Style,
    scale: &dyn Fn(f64) -> f64,
) {
    let xs: Vec<f64> = chart
        .series
        .iter()
        .flat_map(|x| x.x.iter().copied())
        .collect();
    let xlo = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let xhi = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !xlo.is_finite() || !xhi.is_finite() {
        return;
    }
    let span = if (xhi - xlo).abs() < f64::EPSILON {
        1.0
    } else {
        xhi - xlo
    };
    for (si, ser) in chart.series.iter().enumerate() {
        let c = color(chart, si, st);
        for (x, y) in ser.x.iter().zip(&ser.values) {
            let _ = write!(
                s,
                r#"<circle cx="{:.1}" cy="{:.1}" r="4" fill="{c}" fill-opacity="0.8"/>"#,
                px + (x - xlo) / span * pw,
                py + ph - scale(*y) * ph
            );
        }
    }
    let _ = write!(
        s,
        r#"<text x="{px}" y="{}" fill="{}">{}</text><text x="{}" y="{}" text-anchor="end" fill="{}">{}</text>"#,
        py + ph + 16.0,
        st.muted,
        short(xlo),
        px + pw,
        py + ph + 16.0,
        st.muted,
        short(xhi)
    );
}
