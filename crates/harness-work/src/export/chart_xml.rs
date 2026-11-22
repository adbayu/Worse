//! Native `DrawingML` charts (`ppt/charts/chartN.xml`) so `PowerPoint` users can still edit the data.

use std::fmt::Write as _;

use super::ooxml::{esc, hex, XML_HEAD};
use crate::model::chart::{Chart, ChartType, Series};
use crate::render::chart_svg::PALETTE;

fn str_cache(items: &[String]) -> String {
    let mut s = format!(r#"<c:strLit><c:ptCount val="{}"/>"#, items.len());
    for (i, it) in items.iter().enumerate() {
        let _ = write!(s, r#"<c:pt idx="{i}"><c:v>{}</c:v></c:pt>"#, esc(it));
    }
    s + "</c:strLit>"
}

fn num_cache(values: &[f64]) -> String {
    let mut s = format!(
        r#"<c:numLit><c:formatCode>General</c:formatCode><c:ptCount val="{}"/>"#,
        values.len()
    );
    for (i, v) in values.iter().enumerate() {
        let _ = write!(s, r#"<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>"#);
    }
    s + "</c:numLit>"
}

fn fill(color: &str) -> String {
    format!(r#"<a:solidFill><a:srgbClr val="{color}"/></a:solidFill>"#)
}

fn series_color(palette: &[String], s: &Series, i: usize) -> String {
    let base = palette
        .get(i % palette.len().max(1))
        .map_or(PALETTE[0], String::as_str);
    hex(s.color.as_deref().unwrap_or(base), "3B6CF6")
}

fn ser(chart: &Chart, palette: &[String], i: usize, s: &Series, kind: ChartType) -> String {
    let color = series_color(palette, s, i);
    let sp = match kind {
        ChartType::Line => format!(
            r#"<c:spPr><a:ln w="28575">{}</a:ln></c:spPr><c:marker><c:symbol val="none"/></c:marker>"#,
            fill(&color)
        ),
        ChartType::Scatter => format!(
            r#"<c:spPr><a:ln w="19050"><a:noFill/></a:ln></c:spPr><c:marker><c:symbol val="circle"/><c:size val="7"/><c:spPr>{}</c:spPr></c:marker>"#,
            fill(&color)
        ),
        _ => format!("<c:spPr>{}</c:spPr>", fill(&color)),
    };
    let mut points = String::new();
    if matches!(kind, ChartType::Pie | ChartType::Donut) {
        for j in 0..s.values.len() {
            let c = hex(
                palette
                    .get(j % palette.len().max(1))
                    .map_or(PALETTE[0], String::as_str),
                "3B6CF6",
            );
            let _ = write!(
                points,
                r#"<c:dPt><c:idx val="{j}"/><c:bubble3D val="0"/><c:spPr>{}</c:spPr></c:dPt>"#,
                fill(&c)
            );
        }
    }
    let data = if kind == ChartType::Scatter {
        format!(
            "<c:xVal>{}</c:xVal><c:yVal>{}</c:yVal><c:smooth val=\"0\"/>",
            num_cache(&s.x),
            num_cache(&s.values)
        )
    } else {
        let invert = if matches!(kind, ChartType::Bar | ChartType::Column) {
            r#"<c:invertIfNegative val="0"/>"#
        } else {
            ""
        };
        let smooth = if kind == ChartType::Line {
            r#"<c:smooth val="0"/>"#
        } else {
            ""
        };
        format!(
            "{invert}<c:cat>{}</c:cat><c:val>{}</c:val>{smooth}",
            str_cache(&chart.categories),
            num_cache(&s.values)
        )
    };
    let sp = if matches!(kind, ChartType::Pie | ChartType::Donut) {
        String::new()
    } else {
        sp
    };
    format!(
        r#"<c:ser><c:idx val="{i}"/><c:order val="{i}"/><c:tx><c:v>{}</c:v></c:tx>{sp}{points}{data}</c:ser>"#,
        esc(&s.name)
    )
}

const TXT: &str = r#"<c:txPr><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="1200"><a:solidFill><a:srgbClr val="6B7280"/></a:solidFill></a:defRPr></a:pPr><a:endParaRPr lang="en-US"/></a:p></c:txPr>"#;

fn axes(horizontal: bool, scatter: bool) -> String {
    let (cat_pos, val_pos) = if horizontal { ("l", "b") } else { ("b", "l") };
    let grid = r#"<c:majorGridlines><c:spPr><a:ln w="6350"><a:solidFill><a:srgbClr val="E5E7EB"/></a:solidFill></a:ln></c:spPr></c:majorGridlines>"#;
    let first = if scatter {
        format!(
            r#"<c:valAx><c:axId val="1"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="b"/><c:numFmt formatCode="General" sourceLinked="0"/><c:tickLblPos val="nextTo"/>{TXT}<c:crossAx val="2"/><c:crosses val="autoZero"/><c:crossBetween val="midCat"/></c:valAx>"#
        )
    } else {
        format!(
            r#"<c:catAx><c:axId val="1"/><c:scaling><c:orientation val="{}"/></c:scaling><c:delete val="0"/><c:axPos val="{cat_pos}"/><c:numFmt formatCode="General" sourceLinked="0"/><c:tickLblPos val="nextTo"/>{TXT}<c:crossAx val="2"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/><c:lblOffset val="100"/></c:catAx>"#,
            if horizontal { "maxMin" } else { "minMax" }
        )
    };
    format!(
        r#"{first}<c:valAx><c:axId val="2"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="{val_pos}"/>{grid}<c:numFmt formatCode="General" sourceLinked="0"/><c:tickLblPos val="nextTo"/>{TXT}<c:crossAx val="1"/><c:crosses val="autoZero"/><c:crossBetween val="between"/></c:valAx>"#
    )
}

fn group(
    chart: &Chart,
    palette: &[String],
    kind: ChartType,
    series: &[(usize, &Series)],
) -> String {
    let grouping = if chart.stacked {
        "stacked"
    } else if kind == ChartType::Line {
        "standard"
    } else {
        "clustered"
    };
    let sers: String = series
        .iter()
        .map(|(i, s)| ser(chart, palette, *i, s, kind))
        .collect();
    let ax = r#"<c:axId val="1"/><c:axId val="2"/>"#;
    match kind {
        ChartType::Bar | ChartType::Column => {
            let dir = if kind == ChartType::Bar { "bar" } else { "col" };
            let overlap = if chart.stacked {
                r#"<c:overlap val="100"/>"#
            } else {
                ""
            };
            format!(
                r#"<c:barChart><c:barDir val="{dir}"/><c:grouping val="{grouping}"/><c:varyColors val="0"/>{sers}<c:gapWidth val="80"/>{overlap}{ax}</c:barChart>"#
            )
        }
        ChartType::Line => format!(
            r#"<c:lineChart><c:grouping val="{grouping}"/><c:varyColors val="0"/>{sers}<c:marker val="1"/>{ax}</c:lineChart>"#
        ),
        ChartType::Area => format!(
            r#"<c:areaChart><c:grouping val="{}"/><c:varyColors val="0"/>{sers}{ax}</c:areaChart>"#,
            if chart.stacked { "stacked" } else { "standard" }
        ),
        ChartType::Pie => format!(
            r#"<c:pieChart><c:varyColors val="1"/>{sers}<c:firstSliceAng val="0"/></c:pieChart>"#
        ),
        ChartType::Donut => format!(
            r#"<c:doughnutChart><c:varyColors val="1"/>{sers}<c:firstSliceAng val="0"/><c:holeSize val="55"/></c:doughnutChart>"#
        ),
        ChartType::Scatter => format!(
            r#"<c:scatterChart><c:scatterStyle val="lineMarker"/><c:varyColors val="0"/>{sers}{ax}</c:scatterChart>"#
        ),
        ChartType::Combo => String::new(),
    }
}

/// The chart part for a resolved chart (inline data).
pub(crate) fn chart_space(chart: &Chart, palette: &[String]) -> String {
    let round = chart.r#type.is_round();
    let plot = if chart.r#type == ChartType::Combo {
        let bars: Vec<(usize, &Series)> = chart
            .series
            .iter()
            .enumerate()
            .filter(|(_, s)| s.r#type.unwrap_or(ChartType::Column) != ChartType::Line)
            .collect();
        let lines: Vec<(usize, &Series)> = chart
            .series
            .iter()
            .enumerate()
            .filter(|(_, s)| s.r#type == Some(ChartType::Line))
            .collect();
        let mut p = group(chart, palette, ChartType::Column, &bars);
        if !lines.is_empty() {
            p.push_str(&group(chart, palette, ChartType::Line, &lines));
        }
        p
    } else {
        let all: Vec<(usize, &Series)> = chart
            .series
            .iter()
            .enumerate()
            .take(if round { 1 } else { usize::MAX })
            .collect();
        group(chart, palette, chart.r#type, &all)
    };
    let axes = if round {
        String::new()
    } else {
        axes(
            chart.r#type == ChartType::Bar,
            chart.r#type == ChartType::Scatter,
        )
    };
    let title = if chart.title.is_empty() {
        r#"<c:autoTitleDeleted val="1"/>"#.to_string()
    } else {
        format!(
            r#"<c:title><c:tx><c:rich><a:bodyPr/><a:lstStyle/><a:p><a:pPr><a:defRPr sz="1600" b="1"/></a:pPr><a:r><a:rPr lang="en-US" sz="1600" b="1"/><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val="0"/></c:title><c:autoTitleDeleted val="0"/>"#,
            esc(&chart.title)
        )
    };
    let legend = if chart.show_legend() {
        format!(r#"<c:legend><c:legendPos val="b"/><c:overlay val="0"/>{TXT}</c:legend>"#)
    } else {
        String::new()
    };
    format!(
        r#"{XML_HEAD}<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><c:roundedCorners val="0"/><c:chart>{title}<c:plotArea><c:layout/>{plot}{axes}<c:spPr><a:noFill/></c:spPr></c:plotArea>{legend}<c:plotVisOnly val="1"/></c:chart><c:spPr><a:noFill/><a:ln><a:noFill/></a:ln></c:spPr></c:chartSpace>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_types_produce_their_element() {
        let mut c = Chart {
            categories: vec!["a".into(), "b".into()],
            series: vec![Series {
                name: "s".into(),
                values: vec![1.0, 2.0],
                ..Series::default()
            }],
            ..Chart::default()
        };
        let pal: Vec<String> = PALETTE.iter().map(|s| (*s).to_string()).collect();
        for (t, tag) in [
            (ChartType::Column, "c:barChart"),
            (ChartType::Line, "c:lineChart"),
            (ChartType::Pie, "c:pieChart"),
            (ChartType::Donut, "c:doughnutChart"),
            (ChartType::Scatter, "c:scatterChart"),
            (ChartType::Area, "c:areaChart"),
        ] {
            c.r#type = t;
            assert!(chart_space(&c, &pal).contains(tag), "{t:?}");
        }
    }
}
