//! `deck.json` → Typst source. Every layout places its parts as fixed regions on a 1280×720 pt page,
//! so hit boxes are exact and overflow is measured by Typst itself.

use std::collections::HashMap;
use std::fmt::Write as _;

use super::chart_svg;
use super::markup::{color, content, fonts, lit, text, PRELUDE};
use crate::item::WorkItem;
use crate::model::chart::Chart;
use crate::model::deck::{Deck, Element, Layout, Slide, TableData, Theme};

type Rect = (f64, f64, f64, f64);

const TITLE: Rect = (64.0, 44.0, 1152.0, 96.0);
const BODY: Rect = (64.0, 172.0, 1152.0, 488.0);
const LEFT: Rect = (64.0, 172.0, 560.0, 488.0);
const RIGHT: Rect = (656.0, 172.0, 560.0, 488.0);

pub(crate) struct Built {
    pub source: String,
    pub files: HashMap<String, Vec<u8>>,
}

struct Ctx<'a> {
    item: &'a WorkItem,
    theme: &'a Theme,
    out: String,
    files: HashMap<String, Vec<u8>>,
    index: usize,
}

/// Builds the Typst source of the slides in `only` (all when `None`).
pub(crate) fn build(item: &WorkItem, deck: &Deck, only: Option<usize>) -> Built {
    let t = &deck.theme;
    let mut cx = Ctx {
        item,
        theme: t,
        out: String::from(PRELUDE),
        files: HashMap::new(),
        index: 0,
    };
    let _ = writeln!(
        cx.out,
        "#set page(width: 1280pt, height: 720pt, margin: 0pt)\n#set text(font: {}, fill: {}, size: 24pt)\n#set par(leading: 0.55em)\n#let accent = {}\n#let muted = {}\n#let fg = {}",
        fonts(&t.font),
        color(&t.foreground, "#1f2328"),
        color(&t.accent, "#3b6cf6"),
        color(&t.muted, "#6b7280"),
        color(&t.foreground, "#1f2328"),
    );
    let total = deck.slides.len();
    for (i, slide) in deck.slides.iter().enumerate() {
        if only.is_some_and(|o| o != i) {
            continue;
        }
        cx.index = i;
        let bg = if slide.layout == Layout::Section {
            color(&t.accent, "#3b6cf6")
        } else {
            color(&t.background, "#ffffff")
        };
        let _ = writeln!(cx.out, "#page(fill: {bg})[");
        slide_body(&mut cx, slide);
        chrome(&mut cx, slide, i, total);
        cx.out.push_str("]\n");
    }
    if deck.slides.is_empty() || only.is_some_and(|o| o >= total) {
        let _ = writeln!(
            cx.out,
            "#page(fill: {})[#place(center + horizon, text(fill: muted)[#\"Empty deck\"])]",
            color(&t.background, "#ffffff")
        );
    }
    Built {
        source: cx.out,
        files: cx.files,
    }
}

impl Ctx<'_> {
    fn path(&self, field: &str) -> String {
        lit(&format!("deck.json#/slides/{}/{field}", self.index))
    }

    fn region(&mut self, field: &str, r: Rect, body: &str, align: &str) {
        let _ = writeln!(
            self.out,
            "#region({}, {}pt, {}pt, {}pt, {}pt, align-to: {align}, [{body}])",
            self.path(field),
            r.0,
            r.1,
            r.2,
            r.3
        );
    }

    fn title(&mut self, slide: &Slide) {
        if slide.title.is_empty() {
            return;
        }
        let font = self
            .theme
            .heading_font
            .clone()
            .unwrap_or_else(|| self.theme.font.clone());
        let body = format!(
            "#set text(size: 40pt, weight: \"bold\", font: {})\n{}",
            fonts(&font),
            text(&slide.title)
        );
        self.region("title", TITLE, &body, "bottom + left");
        let _ = writeln!(self.out, "#place(top + left, dx: 64pt, dy: 148pt, rect(width: 72pt, height: 5pt, radius: 2pt, fill: accent))");
    }

    fn bullets(&mut self, field: &str, r: Rect, items: &[String], size: f64) {
        let mut body = format!("#set text(size: {size}pt)\n#set list(marker: text(fill: accent)[•], spacing: 0.75em, body-indent: 0.6em)\n");
        for it in items {
            let _ = writeln!(body, "- {}", text(it));
        }
        self.region(field, r, &body, "top + left");
    }

    fn image(&mut self, field: &str, r: Rect, rel: &str) {
        let body = match self.item.asset(rel) {
            Some(p) if p.is_file() => format!("#image({}, width: 100%, height: 100%, fit: \"contain\")", lit(&format!("/{}", rel.trim_start_matches("./")))),
            _ => format!("#rect(width: 100%, height: 100%, stroke: (dash: \"dashed\", paint: muted), fill: none)[#align(center + horizon, text(fill: muted)[#{}])]", lit(&format!("Missing image: {rel}"))),
        };
        self.region(field, r, &body, "center + horizon");
    }

    fn table(&mut self, field: &str, r: Rect, t: &TableData) {
        let cols = t.columns().max(1);
        let size = if t.rows.len() > 8 {
            15
        } else if t.rows.len() > 5 {
            18
        } else {
            21
        };
        let mut body = format!(
            "#set text(size: {size}pt)\n#table(columns: {cols}, inset: 10pt, stroke: none, fill: (x, y) => if y == 0 {{ accent }} else if calc.even(y) {{ accent.lighten(90%) }} else {{ none }},\n"
        );
        let empty = String::new();
        for h in t
            .header
            .iter()
            .chain(std::iter::repeat(&empty).take(cols - t.header.len().min(cols)))
            .take(cols)
        {
            if !t.header.is_empty() {
                let _ = write!(body, "text(fill: white, weight: \"bold\")[{}], ", text(h));
            }
        }
        for row in &t.rows {
            for c in 0..cols {
                let _ = write!(body, "{}, ", content(row.get(c).map_or("", String::as_str)));
            }
        }
        body.push(')');
        self.region(field, r, &body, "top + left");
    }

    fn chart(&mut self, field: &str, r: Rect, chart: &Chart) {
        let mut st = chart_svg::Style {
            fg: &self.theme.foreground,
            muted: &self.theme.muted,
            font: &self.theme.font,
            ..chart_svg::Style::default()
        };
        st.palette.insert(0, self.theme.accent.clone());
        st.palette.dedup();
        let name = format!("__hw/chart-{}-{}.svg", self.index, self.files.len());
        let resolved = super::data::resolve_chart(self.item, chart);
        self.files.insert(
            name.clone(),
            chart_svg::svg(&resolved, r.2, r.3, &st).into_bytes(),
        );
        let body = format!(
            "#image({}, width: 100%, height: 100%)",
            lit(&format!("/{name}"))
        );
        self.region(field, r, &body, "center + horizon");
    }

    fn element(&mut self, j: usize, e: &Element) {
        let field = format!("elements/{j}");
        let r = (e.x, e.y, e.w, e.h);
        match e.kind.as_str() {
            "image" => self.image(&field, r, e.image.as_deref().unwrap_or_default()),
            "table" => self.table(&field, r, e.table.as_ref().unwrap_or(&TableData::default())),
            "chart" => {
                if let Some(c) = &e.chart {
                    self.chart(&field, r, c);
                }
            }
            "shape" => {
                let fill = color(e.fill.as_deref().unwrap_or(&self.theme.accent), "#3b6cf6");
                let body = format!("#rect(width: 100%, height: 100%, radius: 6pt, fill: {fill})[#align(center + horizon, text(fill: white)[{}])]", text(&e.text));
                self.region(&field, r, &body, "center + horizon");
            }
            _ => {
                let fill = e
                    .color
                    .as_deref()
                    .map_or_else(|| "fg".to_string(), |c| color(c, "#1f2328"));
                let body = format!(
                    "#set text(size: {}pt, fill: {fill})\n{}",
                    e.size.unwrap_or(24.0),
                    text(&e.text)
                );
                self.region(&field, r, &body, "top + left");
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn slide_body(cx: &mut Ctx, s: &Slide) {
    match s.layout {
        Layout::Title => {
            let _ = writeln!(cx.out, "#place(top + left, dx: 96pt, dy: 226pt, rect(width: 120pt, height: 7pt, radius: 3pt, fill: accent))");
            let font = cx
                .theme
                .heading_font
                .clone()
                .unwrap_or_else(|| cx.theme.font.clone());
            cx.region(
                "title",
                (96.0, 250.0, 1088.0, 170.0),
                &format!(
                    "#set text(size: 60pt, weight: \"bold\", font: {})\n{}",
                    fonts(&font),
                    text(&s.title)
                ),
                "top + left",
            );
            cx.region(
                "subtitle",
                (96.0, 430.0, 1088.0, 120.0),
                &format!("#set text(size: 28pt, fill: muted)\n{}", text(&s.subtitle)),
                "top + left",
            );
        }
        Layout::Section => {
            cx.region(
                "title",
                (96.0, 250.0, 1088.0, 160.0),
                &format!(
                    "#set text(size: 54pt, weight: \"bold\", fill: white)\n{}",
                    text(&s.title)
                ),
                "bottom + left",
            );
            cx.region(
                "subtitle",
                (96.0, 430.0, 1088.0, 100.0),
                &format!(
                    "#set text(size: 26pt, fill: white.transparentize(20%))\n{}",
                    text(&s.subtitle)
                ),
                "top + left",
            );
        }
        Layout::Bullets => {
            cx.title(s);
            cx.bullets("bullets", BODY, &s.bullets, 26.0);
        }
        Layout::TwoColumn => {
            cx.title(s);
            cx.bullets("left", LEFT, &s.left, 24.0);
            cx.bullets("right", RIGHT, &s.right, 24.0);
        }
        Layout::Image => {
            cx.title(s);
            let r = if s.caption.is_empty() {
                BODY
            } else {
                (64.0, 172.0, 1152.0, 444.0)
            };
            cx.image("image", r, s.image.as_deref().unwrap_or_default());
            if !s.caption.is_empty() {
                cx.region(
                    "caption",
                    (64.0, 628.0, 1152.0, 36.0),
                    &format!("#set text(size: 18pt, fill: muted)\n{}", text(&s.caption)),
                    "center + top",
                );
            }
        }
        Layout::ImageText => {
            cx.title(s);
            cx.image("image", LEFT, s.image.as_deref().unwrap_or_default());
            cx.bullets("bullets", RIGHT, &s.bullets, 24.0);
        }
        Layout::Table => {
            cx.title(s);
            cx.table(
                "table",
                BODY,
                s.table.as_ref().unwrap_or(&TableData::default()),
            );
        }
        Layout::Chart => {
            cx.title(s);
            if let Some(c) = &s.chart {
                cx.chart("chart", BODY, c);
            }
        }
        Layout::Quote => {
            cx.region("quote", (128.0, 150.0, 1024.0, 360.0), &format!("#set text(size: 40pt, style: \"italic\")\n#text(fill: accent, size: 90pt)[“]#linebreak(){}", text(&s.quote)), "left + horizon");
            if !s.by.is_empty() {
                cx.region(
                    "by",
                    (128.0, 530.0, 1024.0, 60.0),
                    &format!("#set text(size: 24pt, fill: muted)\n#\"— \"{}", text(&s.by)),
                    "top + left",
                );
            }
        }
        Layout::BigNumber => {
            cx.title(s);
            cx.region(
                "number",
                (64.0, 190.0, 1152.0, 260.0),
                &format!(
                    "#set text(size: 150pt, weight: \"bold\", fill: accent)\n{}",
                    text(&s.number)
                ),
                "center + bottom",
            );
            cx.region(
                "label",
                (64.0, 470.0, 1152.0, 120.0),
                &format!("#set text(size: 30pt, fill: muted)\n{}", text(&s.label)),
                "center + top",
            );
        }
        Layout::Free => {
            cx.title(s);
            for (j, e) in s.elements.iter().enumerate() {
                cx.element(j, e);
            }
        }
    }
}

fn chrome(cx: &mut Ctx, s: &Slide, i: usize, total: usize) {
    if matches!(s.layout, Layout::Title | Layout::Section) {
        return;
    }
    if let Some(f) = cx.theme.footer.clone().filter(|f| !f.is_empty()) {
        let _ = writeln!(
            cx.out,
            "#place(top + left, dx: 64pt, dy: 680pt, text(size: 13pt, fill: muted)[#{}])",
            lit(&f)
        );
    }
    let _ = writeln!(cx.out, "#place(top + left, dx: 1100pt, dy: 680pt, box(width: 116pt, align(right, text(size: 13pt, fill: muted)[#{}])))", lit(&format!("{} / {total}", i + 1)));
    if let Some(logo) = cx.theme.logo.clone() {
        if cx.item.asset(&logo).is_some_and(|p| p.is_file()) {
            let _ = writeln!(cx.out, "#place(top + left, dx: 1136pt, dy: 30pt, image({}, width: 80pt, height: 40pt, fit: \"contain\"))", lit(&format!("/{logo}")));
        }
    }
}
