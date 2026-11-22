//! Chart data taken from a sheet range (`"data": {"range": "Sheet1!A1:C5"}`): row 1 holds the series
//! names, column 1 the categories. Charts re-render whenever the book changes because the range is
//! read at render time.

use ironcalc::base::cell::CellValue;

use crate::item::{Kind, Source, WorkItem};
use crate::model::chart::{Chart, ChartType, Series};
use crate::sheet::SheetModel;

fn text(v: &CellValue) -> String {
    match v {
        CellValue::None => String::new(),
        CellValue::String(s) => s.clone(),
        CellValue::Number(n) => crate::render::chart_svg::short(*n),
        CellValue::Boolean(b) => b.to_string(),
    }
}

fn number(v: &CellValue) -> f64 {
    match v {
        CellValue::Number(n) => *n,
        CellValue::String(s) => s.trim().replace(',', "").parse().unwrap_or(0.0),
        CellValue::Boolean(b) => f64::from(u8::from(*b)),
        CellValue::None => 0.0,
    }
}

/// Returns the chart with `categories`/`series` filled from its data range (unchanged when it has
/// inline data or the range cannot be read).
#[must_use]
pub fn resolve_chart(item: &WorkItem, chart: &Chart) -> Chart {
    let Some(data) = &chart.data else {
        return chart.clone();
    };
    let book_item = match &data.book {
        Some(rel) => item.asset(rel).and_then(|p| WorkItem::open(&p).ok()),
        None => Some(item.clone()),
    };
    let Some(book_item) = book_item.filter(|b| b.kind == Kind::Book) else {
        return chart.clone();
    };
    let Ok(Source::Book(book)) = book_item.load() else {
        return chart.clone();
    };
    let Ok(model) = SheetModel::from_book(&book) else {
        return chart.clone();
    };
    from_model(&model, chart, &data.range).unwrap_or_else(|| chart.clone())
}

/// Fills a chart from a range of an evaluated sheet.
#[must_use]
pub fn from_model(model: &SheetModel, chart: &Chart, range: &str) -> Option<Chart> {
    let rows = model.range(range).ok()?;
    let header = rows.first()?;
    let mut out = chart.clone();
    let body = &rows[1..];
    if chart.r#type == ChartType::Scatter {
        out.categories.clear();
        out.series = (1..header.len())
            .map(|c| Series {
                name: text(&header[c]),
                x: body.iter().map(|r| number(&r[0])).collect(),
                values: body.iter().map(|r| number(&r[c])).collect(),
                ..Series::default()
            })
            .collect();
        return Some(out);
    }
    out.categories = body.iter().map(|r| text(&r[0])).collect();
    out.series = (1..header.len())
        .map(|c| {
            let old = chart.series.get(c - 1);
            Series {
                name: text(&header[c]),
                values: body.iter().map(|r| r.get(c).map_or(0.0, number)).collect(),
                r#type: old.and_then(|s| s.r#type),
                color: old.and_then(|s| s.color.clone()),
                ..Series::default()
            }
        })
        .collect();
    Some(out)
}
