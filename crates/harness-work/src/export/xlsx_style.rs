//! `xl/styles.xml` for the write-back `.xlsx` export: new fonts, fills, borders, number formats
//! and `cellXfs` entries are appended; existing entries never change, so every untouched cell
//! keeps its exact original format.

use std::fmt::Write as _;

use ironcalc::base::types::{
    Alignment, Border, BorderItem, BorderStyle, Color, HorizontalAlignment, Style,
    VerticalAlignment,
};
use roxmltree::Document;

use super::ooxml::esc;

/// Excel's built-in number formats, which need no `<numFmt>` entry.
const BUILTIN_FORMATS: &[(i32, &str)] = &[
    (0, "general"),
    (1, "0"),
    (2, "0.00"),
    (3, "#,##0"),
    (4, "#,##0.00"),
    (9, "0%"),
    (10, "0.00%"),
    (11, "0.00E+00"),
    (12, "# ?/?"),
    (13, "# ??/??"),
    (14, "mm-dd-yy"),
    (15, "d-mmm-yy"),
    (16, "d-mmm"),
    (17, "mmm-yy"),
    (18, "h:mm AM/PM"),
    (19, "h:mm:ss AM/PM"),
    (20, "h:mm"),
    (21, "h:mm:ss"),
    (22, "m/d/yy h:mm"),
    (45, "mm:ss"),
    (46, "[h]:mm:ss"),
    (47, "mmss.0"),
    (48, "##0.0E+0"),
    (49, "@"),
];

/// Lists that receive new entries, in `TAGS` order.
const TAGS: [&str; 4] = ["fonts", "fills", "borders", "cellXfs"];
const FONTS: usize = 0;
const FILLS: usize = 1;
const BORDERS: usize = 2;
const XFS: usize = 3;

/// The original `styles.xml` plus the entries the edited cells need.
pub(super) struct Styles {
    xml: String,
    counts: [usize; 4],
    num_fmts: Vec<(i32, String)>,
    new_fmts: Vec<(i32, String)>,
    next_fmt: i32,
    added: [Vec<String>; 4],
}

impl Styles {
    pub(super) fn new(xml: String) -> Result<Self, String> {
        let (counts, num_fmts) = {
            let doc = Document::parse(&xml).map_err(|e| format!("styles.xml: {e}"))?;
            let count = |tag: &str| {
                doc.descendants()
                    .find(|n| n.tag_name().name() == tag)
                    .map(|n| n.children().filter(roxmltree::Node::is_element).count())
                    .ok_or_else(|| format!("styles.xml has no <{tag}>"))
            };
            let counts = [
                count(TAGS[0])?,
                count(TAGS[1])?,
                count(TAGS[2])?,
                count(TAGS[3])?,
            ];
            let fmts: Vec<(i32, String)> = doc
                .descendants()
                .filter(|n| n.tag_name().name() == "numFmt")
                .filter_map(|n| {
                    let id = n.attribute("numFmtId")?.parse().ok()?;
                    Some((id, n.attribute("formatCode")?.to_string()))
                })
                .collect();
            (counts, fmts)
        };
        let next_fmt = num_fmts
            .iter()
            .map(|(id, _)| id + 1)
            .max()
            .unwrap_or(0)
            .max(164);
        Ok(Self {
            xml,
            counts,
            num_fmts,
            new_fmts: Vec::new(),
            next_fmt,
            added: Default::default(),
        })
    }

    fn add(&mut self, kind: usize, item: String) -> usize {
        let list = &mut self.added[kind];
        let pos = list.iter().position(|x| *x == item).unwrap_or_else(|| {
            list.push(item);
            list.len() - 1
        });
        self.counts[kind] + pos
    }

    fn num_fmt(&mut self, code: &str) -> i32 {
        let builtin = BUILTIN_FORMATS
            .iter()
            .find(|(_, c)| c.eq_ignore_ascii_case(code));
        let known = self
            .num_fmts
            .iter()
            .chain(&self.new_fmts)
            .find(|(_, c)| c == code);
        if let Some(id) = builtin.map(|(id, _)| *id).or(known.map(|(id, _)| *id)) {
            return id;
        }
        let id = self.next_fmt;
        self.next_fmt += 1;
        self.new_fmts.push((id, code.to_string()));
        id
    }

    /// `cellXfs` index of a cell format equal to `st`.
    pub(super) fn xf(&mut self, st: &Style) -> usize {
        let font = self.add(FONTS, font_xml(st));
        let fill = self.add(FILLS, fill_xml(&st.fill.color));
        let border = self.add(BORDERS, border_xml(&st.border));
        let fmt = self.num_fmt(&st.num_fmt);
        let quote = if st.quote_prefix {
            r#" quotePrefix="1""#
        } else {
            ""
        };
        let attrs = format!(
            r#"numFmtId="{fmt}" fontId="{font}" fillId="{fill}" borderId="{border}" xfId="0"{quote} applyNumberFormat="1" applyFont="1" applyFill="1" applyBorder="1""#
        );
        let xf = match st.alignment.as_ref().map(alignment_xml) {
            Some(a) if !a.is_empty() => format!(r#"<xf {attrs} applyAlignment="1">{a}</xf>"#),
            _ => format!("<xf {attrs}/>"),
        };
        self.add(XFS, xf)
    }

    /// The patched `styles.xml`, or `None` when no cell needed a new format.
    pub(super) fn finish(self) -> Result<Option<String>, String> {
        if self.added[XFS].is_empty() {
            return Ok(None);
        }
        let mut xml = self.xml;
        if !self.new_fmts.is_empty() {
            let mut items = String::new();
            for (id, code) in &self.new_fmts {
                let code = esc(code);
                let _ = write!(items, r#"<numFmt numFmtId="{id}" formatCode="{code}"/>"#);
            }
            if xml.contains("</numFmts>") {
                let total = self.num_fmts.len() + self.new_fmts.len();
                grow(&mut xml, "numFmts", &items, total)?;
            } else {
                // numFmts 必须是 styleSheet 的第一个子元素。
                let start = open_tag(&xml, "styleSheet").ok_or("styles.xml has no styleSheet")?;
                let gt = start + xml[start..].find('>').ok_or("bad styleSheet tag")? + 1;
                let block = format!(
                    r#"<numFmts count="{}">{items}</numFmts>"#,
                    self.new_fmts.len()
                );
                xml.insert_str(gt, &block);
            }
        }
        for (kind, tag) in TAGS.iter().enumerate() {
            let added = &self.added[kind];
            if !added.is_empty() {
                grow(
                    &mut xml,
                    tag,
                    &added.concat(),
                    self.counts[kind] + added.len(),
                )?;
            }
        }
        Ok(Some(xml))
    }
}

/// Start of the `<tag ...>` element (not `<tagX`).
fn open_tag(xml: &str, tag: &str) -> Option<usize> {
    let pat = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&pat).map(|i| from + i) {
        if matches!(xml[i + pat.len()..].chars().next(), Some(' ' | '>' | '/')) {
            return Some(i);
        }
        from = i + pat.len();
    }
    None
}

/// Appends `items` inside `<tag>` and sets its `count`.
fn grow(xml: &mut String, tag: &str, items: &str, count: usize) -> Result<(), String> {
    let open = open_tag(xml, tag).ok_or_else(|| format!("styles.xml has no <{tag}>"))?;
    let close = xml[open..]
        .find(&format!("</{tag}>"))
        .ok_or_else(|| format!("styles.xml has an empty <{tag}/>"))?;
    xml.insert_str(open + close, items);
    let gt = open + xml[open..].find('>').ok_or("bad tag")?;
    let head = &xml[open..gt];
    let head = match head.find(" count=\"") {
        Some(i) => {
            let start = i + " count=\"".len();
            let end = start + head[start..].find('"').ok_or("bad count")?;
            format!("{}{count}{}", &head[..start], &head[end..])
        }
        None => format!(r#"{head} count="{count}""#),
    };
    xml.replace_range(open..gt, &head);
    Ok(())
}

fn color_xml(c: &Color, name: &str) -> String {
    match c {
        Color::Rgb(hex) => format!(r#"<{name} rgb="FF{}"/>"#, hex.trim_start_matches('#')),
        Color::Theme(i, tint) if tint.abs() < f64::EPSILON => format!(r#"<{name} theme="{i}"/>"#),
        Color::Theme(i, tint) => format!(r#"<{name} theme="{i}" tint="{tint}"/>"#),
        Color::None => String::new(),
    }
}

fn font_xml(st: &Style) -> String {
    let f = &st.font;
    let flag = |on: bool, tag: &str| {
        if on {
            format!("<{tag}/>")
        } else {
            String::new()
        }
    };
    format!(
        r#"<font>{}{}{}{}<sz val="{}"/>{}<name val="{}"/><family val="{}"/><scheme val="{}"/></font>"#,
        flag(f.b, "b"),
        flag(f.i, "i"),
        flag(f.strike, "strike"),
        flag(f.u, "u"),
        f.sz,
        color_xml(&f.color, "color"),
        esc(&f.name),
        f.family,
        f.scheme
    )
}

fn fill_xml(color: &Color) -> String {
    if matches!(color, Color::None) {
        return r#"<fill><patternFill patternType="none"/></fill>"#.into();
    }
    format!(
        r#"<fill><patternFill patternType="solid">{}<bgColor indexed="64"/></patternFill></fill>"#,
        color_xml(color, "fgColor")
    )
}

/// OOXML `ST_BorderStyle` name; `IronCalc`'s `Display` writes `mediumdashed`, which Excel rejects.
fn border_style(s: &BorderStyle) -> &'static str {
    match s {
        BorderStyle::Thin => "thin",
        BorderStyle::Thick => "thick",
        BorderStyle::SlantDashDot => "slantDashDot",
        BorderStyle::MediumDashed => "mediumDashed",
        BorderStyle::MediumDashDotDot => "mediumDashDotDot",
        BorderStyle::MediumDashDot => "mediumDashDot",
        BorderStyle::Medium => "medium",
        BorderStyle::Double => "double",
        BorderStyle::Dotted => "dotted",
    }
}

fn border_xml(b: &Border) -> String {
    let side = |item: &Option<BorderItem>, tag: &str| match item {
        Some(i) => format!(
            r#"<{tag} style="{}">{}</{tag}>"#,
            border_style(&i.style),
            color_xml(&i.color, "color")
        ),
        None => format!("<{tag}/>"),
    };
    let up = if b.diagonal_up {
        r#" diagonalUp="1""#
    } else {
        ""
    };
    let down = if b.diagonal_down {
        r#" diagonalDown="1""#
    } else {
        ""
    };
    format!(
        "<border{up}{down}>{}{}{}{}{}</border>",
        side(&b.left, "left"),
        side(&b.right, "right"),
        side(&b.top, "top"),
        side(&b.bottom, "bottom"),
        side(&b.diagonal, "diagonal")
    )
}

fn alignment_xml(a: &Alignment) -> String {
    let mut attrs = String::new();
    if a.horizontal != HorizontalAlignment::default() {
        let _ = write!(attrs, r#" horizontal="{}""#, a.horizontal);
    }
    if a.vertical != VerticalAlignment::default() {
        let _ = write!(attrs, r#" vertical="{}""#, a.vertical);
    }
    if a.wrap_text {
        attrs.push_str(r#" wrapText="1""#);
    }
    if attrs.is_empty() {
        String::new()
    } else {
        format!("<alignment{attrs}/>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_cell_formats() {
        let xml = r#"<styleSheet xmlns="x"><fonts count="1"><font/></fonts><fills count="2"><fill/><fill/></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs></styleSheet>"#;
        let mut styles = Styles::new(xml.into()).unwrap();
        let mut st = Style::default();
        st.font.b = true;
        st.fill.color = Color::Rgb("#FFFF00".into());
        st.num_fmt = "#,##0.000".into();
        assert_eq!(styles.xf(&st), 1);
        assert_eq!(styles.xf(&st), 1);
        st.font.b = false;
        assert_eq!(styles.xf(&st), 2);
        let out = styles.finish().unwrap().unwrap();
        let fmts =
            r##"<numFmts count="1"><numFmt numFmtId="164" formatCode="#,##0.000"/></numFmts>"##;
        assert!(out.contains(&format!("{fmts}<fonts")), "{out}");
        assert!(out.contains(r#"<fonts count="3">"#), "{out}");
        assert!(out.contains(r#"<fills count="3">"#), "{out}");
        assert!(out.contains(r#"<cellXfs count="3">"#), "{out}");
        assert!(out.contains(r#"<fgColor rgb="FFFFFF00"/>"#), "{out}");
        assert!(
            out.contains(r#"numFmtId="164" fontId="1" fillId="2""#),
            "{out}"
        );
    }
}
