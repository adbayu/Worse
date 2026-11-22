//! `PresentationML` shape snippets: text boxes with rich runs, bullets, tables, pictures, chart frames.

use std::fmt::Write as _;

use super::ooxml::{esc, hex};
use crate::model::deck::TableData;
use crate::model::text::runs;

pub(crate) const EMU: f64 = 12700.0;

pub(crate) type Rect = (f64, f64, f64, f64);

#[allow(clippy::cast_possible_truncation)]
fn emu(pt: f64) -> i64 {
    (pt * EMU).round() as i64
}

fn xfrm(r: Rect) -> String {
    format!(
        r#"<a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/>"#,
        emu(r.0),
        emu(r.1),
        emu(r.2),
        emu(r.3)
    )
}

/// Text style for runs.
#[derive(Clone)]
pub(crate) struct Font {
    pub face: String,
    pub size: f64,
    pub color: String,
    pub bold: bool,
    pub italic: bool,
}

#[allow(clippy::cast_possible_truncation)]
fn rich(src: &str, f: &Font) -> String {
    let mut out = String::new();
    for r in runs(src) {
        let face = if r.code { "Consolas" } else { f.face.as_str() };
        let _ = write!(
            out,
            r#"<a:r><a:rPr lang="en-US" sz="{}" b="{}" i="{}" dirty="0"><a:solidFill><a:srgbClr val="{}"/></a:solidFill><a:latin typeface="{}"/></a:rPr><a:t>{}</a:t></a:r>"#,
            (f.size * 100.0).round() as i64,
            u8::from(f.bold || r.bold),
            u8::from(f.italic || r.italic),
            hex(&f.color, "1F2328"),
            esc(face),
            esc(&r.text)
        );
    }
    out
}

/// A paragraph (`algn` = `l`, `ctr`, `r`).
pub(crate) fn para(src: &str, f: &Font, algn: &str) -> String {
    format!(
        r#"<a:p><a:pPr algn="{algn}"/>{}<a:endParaRPr lang="en-US"/></a:p>"#,
        rich(src, f)
    )
}

pub(crate) fn bullet(src: &str, f: &Font, marker: &str) -> String {
    format!(
        r#"<a:p><a:pPr marL="342900" indent="-342900"><a:spcBef><a:spcPts val="900"/></a:spcBef><a:buClr><a:srgbClr val="{}"/></a:buClr><a:buFont typeface="Arial"/><a:buChar char="&#8226;"/></a:pPr>{}</a:p>"#,
        hex(marker, "3B6CF6"),
        rich(src, f)
    )
}

/// A text box; `anchor` = `t`, `ctr` or `b`.
/// Title, subtitle and body boxes are placeholders so `PowerPoint` (and re-import) know their role.
pub(crate) fn text_box(id: u32, name: &str, r: Rect, paras: &str, anchor: &str) -> String {
    let ph = match name {
        "Title" => r#"<p:ph type="title"/>"#,
        "Center Title" => r#"<p:ph type="ctrTitle"/>"#,
        "Subtitle" => r#"<p:ph type="subTitle" idx="1"/>"#,
        "Body" => r#"<p:ph idx="1"/>"#,
        "Footer" => r#"<p:ph type="ftr" sz="quarter" idx="11"/>"#,
        "Slide number" => r#"<p:ph type="sldNum" sz="quarter" idx="12"/>"#,
        _ => "",
    };
    let (sp_pr, nv) = if ph.is_empty() {
        (r#"<p:cNvSpPr txBox="1"/>"#, "<p:nvPr/>".to_string())
    } else {
        (
            r#"<p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr>"#,
            format!("<p:nvPr>{ph}</p:nvPr>"),
        )
    };
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="{}"/>{sp_pr}{nv}</p:nvSpPr><p:spPr><a:xfrm>{}</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/></p:spPr><p:txBody><a:bodyPr wrap="square" lIns="0" tIns="0" rIns="0" bIns="0" anchor="{anchor}"><a:normAutofit/></a:bodyPr><a:lstStyle/>{paras}</p:txBody></p:sp>"#,
        esc(name),
        xfrm(r)
    )
}

/// A filled (rounded) rectangle with optional centered text.
pub(crate) fn shape(id: u32, r: Rect, fill: &str, radius: bool, paras: &str) -> String {
    let geom = if radius { "roundRect" } else { "rect" };
    let body = if paras.is_empty() {
        "<a:p><a:endParaRPr lang=\"en-US\"/></a:p>"
    } else {
        paras
    };
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="{id}" name="Shape {id}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm>{}</a:xfrm><a:prstGeom prst="{geom}"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="{}"/></a:solidFill><a:ln><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr anchor="ctr"/><a:lstStyle/>{body}</p:txBody></p:sp>"#,
        xfrm(r),
        hex(fill, "3B6CF6")
    )
}

fn cell(text: &str, f: &Font, fill: Option<&str>) -> String {
    let fill = fill.map_or_else(
        || "<a:noFill/>".to_string(),
        |c| format!(r#"<a:solidFill><a:srgbClr val="{c}"/></a:solidFill>"#),
    );
    format!(
        r#"<a:tc><a:txBody><a:bodyPr/><a:lstStyle/>{}</a:txBody><a:tcPr marL="91440" marR="91440" marT="45720" marB="45720">{fill}</a:tcPr></a:tc>"#,
        para(text, f, "l")
    )
}

/// A native table: accent header row, banded body rows.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn table(id: u32, r: Rect, t: &TableData, f: &Font, accent: &str) -> String {
    let cols = t.columns().max(1);
    let rows = t.rows.len() + usize::from(!t.header.is_empty());
    let size = if t.rows.len() > 8 {
        14.0
    } else if t.rows.len() > 5 {
        16.0
    } else {
        18.0
    };
    let body_font = Font { size, ..f.clone() };
    let head_font = Font {
        size,
        bold: true,
        color: "#ffffff".into(),
        ..f.clone()
    };
    let col_w = emu(r.2 / cols as f64);
    let row_h = emu((r.3 / rows.max(1) as f64).min(48.0));
    let mut grid = String::new();
    for _ in 0..cols {
        let _ = write!(grid, r#"<a:gridCol w="{col_w}"/>"#);
    }
    let accent = hex(accent, "3B6CF6");
    let mut body = String::new();
    if !t.header.is_empty() {
        let _ = write!(body, r#"<a:tr h="{row_h}">"#);
        for c in 0..cols {
            body.push_str(&cell(
                t.header.get(c).map_or("", String::as_str),
                &head_font,
                Some(&accent),
            ));
        }
        body.push_str("</a:tr>");
    }
    for (i, row) in t.rows.iter().enumerate() {
        let _ = write!(body, r#"<a:tr h="{row_h}">"#);
        let band = (i % 2 == 1).then_some("F3F4F6");
        for c in 0..cols {
            body.push_str(&cell(
                row.get(c).map_or("", String::as_str),
                &body_font,
                band,
            ));
        }
        body.push_str("</a:tr>");
    }
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Table {id}"/><p:cNvGraphicFramePr><a:graphicFrameLocks noGrp="1"/></p:cNvGraphicFramePr><p:nvPr/></p:nvGraphicFramePr><p:xfrm>{}</p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1" bandRow="1"/><a:tblGrid>{grid}</a:tblGrid>{body}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
        xfrm(r)
    )
}

/// Width and height in pixels of a PNG or JPEG.
pub(crate) fn image_size(bytes: &[u8]) -> Option<(f64, f64)> {
    if bytes.starts_with(b"\x89PNG") && bytes.len() > 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((f64::from(w), f64::from(h)));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]);
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]);
                return Some((f64::from(w), f64::from(h)));
            }
            i += 2 + len;
        }
    }
    None
}

/// Fits `size` into `r` keeping the aspect ratio, centered.
pub(crate) fn contain(r: Rect, size: Option<(f64, f64)>) -> Rect {
    let Some((w, h)) = size.filter(|(w, h)| *w > 0.0 && *h > 0.0) else {
        return r;
    };
    let k = (r.2 / w).min(r.3 / h);
    let (nw, nh) = (w * k, h * k);
    (r.0 + (r.2 - nw) / 2.0, r.1 + (r.3 - nh) / 2.0, nw, nh)
}

pub(crate) fn picture(id: u32, rid: &str, r: Rect) -> String {
    format!(
        r#"<p:pic><p:nvPicPr><p:cNvPr id="{id}" name="Picture {id}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm>{}</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>"#,
        xfrm(r)
    )
}

pub(crate) fn chart_frame(id: u32, rid: &str, r: Rect) -> String {
    format!(
        r#"<p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="{id}" name="Chart {id}"/><p:cNvGraphicFramePr/><p:nvPr/></p:nvGraphicFramePr><p:xfrm>{}</p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="{rid}"/></a:graphicData></a:graphic></p:graphicFrame>"#,
        xfrm(r)
    )
}
