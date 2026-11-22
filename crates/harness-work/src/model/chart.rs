//! `chart.json` (`harness-chart/1`) and the chart object embedded in decks, books and docs.

use serde::{Deserialize, Serialize};

pub const FORMAT: &str = "harness-chart/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChartType {
    #[default]
    Bar,
    Column,
    Line,
    Area,
    Pie,
    Donut,
    Scatter,
    Combo,
}

impl ChartType {
    #[must_use]
    pub fn is_round(self) -> bool {
        matches!(self, Self::Pie | Self::Donut)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub name: String,
    #[serde(default)]
    pub values: Vec<f64>,
    /// Scatter only: x values (same length as `values`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<f64>,
    /// Combo only: `bar` or `line` for this series.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<ChartType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Data taken from a sheet: `range` like `Sheet1!A1:C5`. Row 1 holds series names, column 1 the
/// categories. `book` is a path relative to the item folder (default: the item's own `book.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataRef {
    pub range: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Chart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default)]
    pub r#type: ChartType,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub series: Vec<Series>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<DataRef>,
    #[serde(default)]
    pub stacked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_label: Option<String>,
    /// Legend on by default when there is more than one series (or for pie charts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legend: Option<bool>,
}

impl Chart {
    #[must_use]
    pub fn show_legend(&self) -> bool {
        self.legend
            .unwrap_or(self.series.len() > 1 || self.r#type.is_round())
    }

    /// Largest and smallest value over every series (0 always included).
    #[must_use]
    pub fn value_range(&self) -> (f64, f64) {
        let mut lo = 0.0_f64;
        let mut hi = 0.0_f64;
        if self.stacked {
            for i in 0..self.categories.len() {
                let (mut pos, mut neg) = (0.0, 0.0);
                for s in &self.series {
                    let v = s.values.get(i).copied().unwrap_or(0.0);
                    if v >= 0.0 {
                        pos += v;
                    } else {
                        neg += v;
                    }
                }
                hi = hi.max(pos);
                lo = lo.min(neg);
            }
        } else {
            for v in self.series.iter().flat_map(|s| s.values.iter()) {
                hi = hi.max(*v);
                lo = lo.min(*v);
            }
        }
        if (hi - lo).abs() < f64::EPSILON {
            hi = lo + 1.0;
        }
        (lo, hi)
    }
}
