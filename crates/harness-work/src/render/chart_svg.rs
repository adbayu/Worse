//! Charts drawn as SVG (bar, column, line, area, pie, donut, scatter, combo) with axis, gridlines,
//! legend and labels. Typst embeds the SVG for previews, PDF and docx images; .xlsx/.pptx get native
//! chart XML instead (`export/chart_xml.rs`).

use std::fmt::Write as _;

use super::chart_svg_axes::axes_chart;
use crate::model::chart::{Chart, ChartType};

/// Default palette (accent first); brand colors replace it when given.
pub const PALETTE: [&str; 8] = [
    "#3b6cf6", "#f59e0b", "#10b981", "#ef4444", "#8b5cf6", "#06b6d4", "#ec4899", "#84cc16",
];

pub struct Style<'a> {
    pub fg: &'a str,
    pub muted: &'a str,
    pub grid: &'a str,
    pub font: &'a str,
    pub palette: Vec<String>,
}

impl Default for Style<'_> {
    fn default() -> Self {
        Self {
            fg: "#1f2328",
            muted: "#6b7280",
            grid: "#e5e7eb",
            font: "Segoe UI",
            palette: PALETTE.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

pub(super) fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Short number label (1.2k, 3.4M).
#[must_use]
pub fn short(v: f64) -> String {
    let a = v.abs();
    let (n, unit) = if a >= 1e9 {
        (v / 1e9, "B")
    } else if a >= 1e6 {
        (v / 1e6, "M")
    } else if a >= 1e4 {
        (v / 1e3, "k")
    } else {
        (v, "")
    };
    let s = if (n - n.round()).abs() < 1e-9 {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    };
    format!("{s}{unit}")
}

/// A "nice" axis step for the range.
pub(super) fn nice_step(span: f64) -> f64 {
    let raw = span / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let f = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    f * mag
}

#[must_use]
pub fn svg(chart: &Chart, w: f64, h: f64, st: &Style) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" font-family="{}" font-size="13">"#,
        esc(st.font)
    );
    let mut top = 8.0;
    if !chart.title.is_empty() {
        let _ = write!(
            s,
            r#"<text x="{}" y="22" text-anchor="middle" font-size="16" font-weight="bold" fill="{}">{}</text>"#,
            w / 2.0,
            st.fg,
            esc(&chart.title)
        );
        top = 36.0;
    }
    let legend = chart.show_legend();
    let bottom = if legend { h - 30.0 } else { h - 8.0 };
    if chart.r#type.is_round() {
        pie(&mut s, chart, (w, top, bottom), st);
    } else {
        axes_chart(&mut s, chart, (w, top, bottom), st);
    }
    if legend {
        legend_row(&mut s, chart, w, h - 12.0, st);
    }
    s.push_str("</svg>");
    s
}

pub(super) fn color(chart: &Chart, i: usize, st: &Style) -> String {
    chart
        .series
        .get(i)
        .and_then(|s| s.color.clone())
        .unwrap_or_else(|| {
            st.palette
                .get(i % st.palette.len().max(1))
                .cloned()
                .unwrap_or_else(|| PALETTE[0].into())
        })
}

#[allow(clippy::cast_precision_loss, clippy::many_single_char_names)]
fn legend_row(s: &mut String, chart: &Chart, w: f64, y: f64, st: &Style) {
    let names: Vec<String> = if chart.r#type.is_round() {
        chart.categories.clone()
    } else {
        chart.series.iter().map(|x| x.name.clone()).collect()
    };
    let widths: Vec<f64> = names
        .iter()
        .map(|n| 22.0 + 7.0 * n.chars().count() as f64)
        .collect();
    let total: f64 = widths.iter().sum();
    let mut x = ((w - total) / 2.0).max(4.0);
    for (i, (n, wd)) in names.iter().zip(widths).enumerate() {
        let c = if chart.r#type.is_round() {
            st.palette
                .get(i % st.palette.len().max(1))
                .cloned()
                .unwrap_or_default()
        } else {
            color(chart, i, st)
        };
        let _ = write!(
            s,
            r#"<rect x="{x}" y="{}" width="10" height="10" rx="2" fill="{c}"/><text x="{}" y="{}" fill="{}">{}</text>"#,
            y - 9.0,
            x + 14.0,
            y,
            st.muted,
            esc(n)
        );
        x += wd;
    }
}

fn pie(s: &mut String, chart: &Chart, (w, top, bottom): (f64, f64, f64), st: &Style) {
    let values: Vec<f64> = chart
        .series
        .first()
        .map(|x| x.values.clone())
        .unwrap_or_default();
    let total: f64 = values.iter().filter(|v| **v > 0.0).sum();
    if total <= 0.0 {
        return;
    }
    let r = ((bottom - top) / 2.0 - 4.0).min(w / 2.0 - 8.0).max(10.0);
    let (cx, cy) = (w / 2.0, top + (bottom - top) / 2.0);
    let mut a0 = -std::f64::consts::FRAC_PI_2;
    for (i, v) in values.iter().enumerate() {
        if *v <= 0.0 {
            continue;
        }
        let a1 = a0 + v / total * std::f64::consts::TAU;
        let large = u8::from(a1 - a0 > std::f64::consts::PI);
        let (x0, y0) = (cx + r * a0.cos(), cy + r * a0.sin());
        let (x1, y1) = (cx + r * a1.cos(), cy + r * a1.sin());
        let c = st
            .palette
            .get(i % st.palette.len().max(1))
            .cloned()
            .unwrap_or_default();
        if (v / total - 1.0).abs() < 1e-9 {
            let _ = write!(s, r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{c}"/>"#);
        } else {
            let _ = write!(
                s,
                r#"<path d="M{cx},{cy} L{x0:.2},{y0:.2} A{r},{r} 0 {large} 1 {x1:.2},{y1:.2} Z" fill="{c}" stroke="white" stroke-width="1.5"/>"#
            );
        }
        let mid = (a0 + a1) / 2.0;
        let pct = v / total * 100.0;
        if pct >= 4.0 {
            let lr = if chart.r#type == ChartType::Donut {
                r * 0.78
            } else {
                r * 0.62
            };
            let _ = write!(
                s,
                r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" fill="white" font-weight="bold">{pct:.0}%</text>"#,
                cx + lr * mid.cos(),
                cy + lr * mid.sin() + 4.0
            );
        }
        a0 = a1;
    }
    if chart.r#type == ChartType::Donut {
        let _ = write!(
            s,
            r#"<circle cx="{cx}" cy="{cy}" r="{}" fill="white"/><text x="{cx}" y="{}" text-anchor="middle" font-size="18" font-weight="bold" fill="{}">{}</text>"#,
            r * 0.55,
            cy + 6.0,
            st.fg,
            short(total)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chart::Series;

    #[test]
    fn draws_every_type() {
        for t in [
            ChartType::Bar,
            ChartType::Column,
            ChartType::Line,
            ChartType::Area,
            ChartType::Pie,
            ChartType::Donut,
            ChartType::Combo,
        ] {
            let c = Chart {
                r#type: t,
                title: "Revenue".into(),
                categories: vec!["Q1".into(), "Q2".into()],
                series: vec![
                    Series {
                        name: "A".into(),
                        values: vec![1.0, 2.0],
                        ..Series::default()
                    },
                    Series {
                        name: "B".into(),
                        values: vec![3.0, 1.5],
                        ..Series::default()
                    },
                ],
                ..Chart::default()
            };
            let out = svg(&c, 600.0, 340.0, &Style::default());
            assert!(out.starts_with("<svg") && out.ends_with("</svg>"), "{t:?}");
            assert!(out.contains("Revenue"));
        }
        assert_eq!(short(12_500.0), "12.5k");
    }
}
