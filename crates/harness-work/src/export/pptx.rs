//! `deck.json` → `.pptx` with native text boxes, tables and charts (editable in `PowerPoint`), using
//! the same regions as the Typst slide renderer.

use super::chart_xml::chart_space;
use super::ooxml::{esc, hex, rels, Package, XML_HEAD};
use super::pptx_shapes::{
    bullet, chart_frame, contain, image_size, para, picture, shape, text_box, Font, Rect, EMU,
};
use crate::error::WorkResult;
use crate::item::WorkItem;
use crate::model::chart::Chart;
use crate::model::deck::{Deck, Layout, Slide, Theme, HEIGHT, WIDTH};
use crate::render::chart_svg::PALETTE;

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const CT: &str = "application/vnd.openxmlformats-officedocument";

const TITLE: Rect = (64.0, 44.0, 1152.0, 96.0);
pub(super) const BODY: Rect = (64.0, 172.0, 1152.0, 488.0);
pub(super) const LEFT: Rect = (64.0, 172.0, 560.0, 488.0);
pub(super) const RIGHT: Rect = (656.0, 172.0, 560.0, 488.0);

pub(super) struct Out<'a> {
    pub(super) item: &'a WorkItem,
    pub(super) theme: &'a Theme,
    pub(super) pkg: Package,
    pub(super) shapes: String,
    pub(super) rels: Vec<(String, &'static str, String)>,
    pub(super) id: u32,
    pub(super) media: usize,
    pub(super) charts: usize,
}

impl Out<'_> {
    pub(super) fn next(&mut self) -> u32 {
        self.id += 1;
        self.id
    }

    pub(super) fn font(&self, size: f64) -> Font {
        Font {
            face: self.theme.font.clone(),
            size,
            color: self.theme.foreground.clone(),
            bold: false,
            italic: false,
        }
    }

    pub(super) fn text(&mut self, name: &str, r: Rect, paras: &str, anchor: &str) {
        let id = self.next();
        self.shapes.push_str(&text_box(id, name, r, paras, anchor));
    }

    pub(super) fn title(&mut self, s: &Slide) {
        if s.title.is_empty() {
            return;
        }
        let f = Font {
            face: self
                .theme
                .heading_font
                .clone()
                .unwrap_or_else(|| self.theme.font.clone()),
            bold: true,
            ..self.font(40.0)
        };
        self.text("Title", TITLE, &para(&s.title, &f, "l"), "b");
        let id = self.next();
        self.shapes.push_str(&shape(
            id,
            (64.0, 148.0, 72.0, 5.0),
            &self.theme.accent,
            true,
            "",
        ));
    }

    pub(super) fn bullets(&mut self, r: Rect, items: &[String], size: f64) {
        let f = self.font(size);
        let paras: String = items
            .iter()
            .map(|b| bullet(b, &f, &self.theme.accent))
            .collect();
        self.text("Body", r, &paras, "t");
    }

    pub(super) fn image(&mut self, r: Rect, rel: &str) {
        let Some(bytes) = self.item.asset(rel).and_then(|p| std::fs::read(p).ok()) else {
            let f = Font {
                color: self.theme.muted.clone(),
                ..self.font(18.0)
            };
            self.text(
                "Missing image",
                r,
                &para(&format!("Missing image: {rel}"), &f, "ctr"),
                "ctr",
            );
            return;
        };
        let ext = rel.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
        self.media += 1;
        let name = format!("ppt/media/image{}.{ext}", self.media);
        if self.pkg.media(&name, &bytes).is_err() {
            return;
        }
        let rid = format!("rId{}", self.rels.len() + 2);
        self.rels.push((
            rid.clone(),
            "image",
            format!("../media/image{}.{ext}", self.media),
        ));
        let id = self.next();
        self.shapes
            .push_str(&picture(id, &rid, contain(r, image_size(&bytes))));
    }

    pub(super) fn chart(&mut self, r: Rect, chart: &Chart) {
        let resolved = crate::render::data::resolve_chart(self.item, chart);
        let mut palette: Vec<String> = PALETTE.iter().map(|s| (*s).to_string()).collect();
        palette.insert(0, self.theme.accent.clone());
        palette.dedup();
        self.charts += 1;
        let name = format!("ppt/charts/chart{}.xml", self.charts);
        if self
            .pkg
            .part(
                &name,
                &format!("{CT}.drawingml.chart+xml"),
                chart_space(&resolved, &palette).as_bytes(),
            )
            .is_err()
        {
            return;
        }
        let rid = format!("rId{}", self.rels.len() + 2);
        self.rels.push((
            rid.clone(),
            "chart",
            format!("../charts/chart{}.xml", self.charts),
        ));
        let id = self.next();
        self.shapes.push_str(&chart_frame(id, &rid, r));
    }
}

#[allow(clippy::cast_possible_truncation)]
fn size_emu(pt: f64) -> i64 {
    (pt * EMU).round() as i64
}

/// Writes the deck as a `.pptx` file body.
#[allow(clippy::too_many_lines)]
pub(crate) fn pptx(item: &WorkItem, deck: &Deck) -> WorkResult<Vec<u8>> {
    let mut out = Out {
        item,
        theme: &deck.theme,
        pkg: Package::new(),
        shapes: String::new(),
        rels: Vec::new(),
        id: 1,
        media: 0,
        charts: 0,
    };
    let n = deck.slides.len();
    out.pkg.put(
        "_rels/.rels",
        rels(&[(
            "rId1".into(),
            "officeDocument",
            "ppt/presentation.xml".into(),
        )])
        .as_bytes(),
    )?;
    let mut pres_rels = vec![
        (
            "rId1".to_string(),
            "slideMaster",
            "slideMasters/slideMaster1.xml".to_string(),
        ),
        ("rId2".into(), "theme", "theme/theme1.xml".into()),
    ];
    let mut ids = String::new();
    for i in 0..n {
        use std::fmt::Write as _;
        pres_rels.push((
            format!("rId{}", i + 3),
            "slide",
            format!("slides/slide{}.xml", i + 1),
        ));
        let _ = write!(ids, r#"<p:sldId id="{}" r:id="rId{}"/>"#, 256 + i, i + 3);
    }
    let (w, h) = (size_emu(WIDTH), size_emu(HEIGHT));
    let pres = format!(
        r#"{XML_HEAD}<p:presentation {NS}><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx="{w}" cy="{h}"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    );
    out.pkg.part(
        "ppt/presentation.xml",
        &format!("{CT}.presentationml.presentation.main+xml"),
        pres.as_bytes(),
    )?;
    out.pkg.put(
        "ppt/_rels/presentation.xml.rels",
        rels(&pres_rels).as_bytes(),
    )?;
    out.pkg.part(
        "ppt/theme/theme1.xml",
        &format!("{CT}.theme+xml"),
        theme_xml(&deck.theme).as_bytes(),
    )?;
    let tree = r#"<p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree>"#;
    out.pkg.part("ppt/slideMasters/slideMaster1.xml", &format!("{CT}.presentationml.slideMaster+xml"), format!(r#"{XML_HEAD}<p:sldMaster {NS}><p:cSld>{tree}</p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#).as_bytes())?;
    out.pkg.put(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        rels(&[
            (
                "rId1".into(),
                "slideLayout",
                "../slideLayouts/slideLayout1.xml".into(),
            ),
            ("rId2".into(), "theme", "../theme/theme1.xml".into()),
        ])
        .as_bytes(),
    )?;
    out.pkg.part("ppt/slideLayouts/slideLayout1.xml", &format!("{CT}.presentationml.slideLayout+xml"), format!(r#"{XML_HEAD}<p:sldLayout {NS} type="blank"><p:cSld name="Blank">{tree}</p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#).as_bytes())?;
    out.pkg.put(
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        rels(&[(
            "rId1".into(),
            "slideMaster",
            "../slideMasters/slideMaster1.xml".into(),
        )])
        .as_bytes(),
    )?;
    for (i, s) in deck.slides.iter().enumerate() {
        out.shapes.clear();
        out.rels = vec![(
            "rId1".into(),
            "slideLayout",
            "../slideLayouts/slideLayout1.xml".into(),
        )];
        out.id = 1;
        out.body(s);
        out.chrome(s, i, n);
        let bg = if s.layout == Layout::Section {
            hex(&deck.theme.accent, "3B6CF6")
        } else {
            hex(&deck.theme.background, "FFFFFF")
        };
        let xml = format!(
            r#"{XML_HEAD}<p:sld {NS}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="{bg}"/></a:solidFill><a:effectLst/></p:bgPr></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
            out.shapes
        );
        out.pkg.part(
            &format!("ppt/slides/slide{}.xml", i + 1),
            &format!("{CT}.presentationml.slide+xml"),
            xml.as_bytes(),
        )?;
        let r = rels(&out.rels);
        out.pkg.put(
            &format!("ppt/slides/_rels/slide{}.xml.rels", i + 1),
            r.as_bytes(),
        )?;
    }
    out.pkg.finish()
}

fn theme_xml(t: &Theme) -> String {
    let c = |v: &str, f: &str| hex(v, f);
    let font = esc(&t.font);
    let head = esc(t.heading_font.as_deref().unwrap_or(&t.font));
    let fills = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>".repeat(3);
    let lines =
        r#"<a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>"#.repeat(3);
    let effects = "<a:effectStyle><a:effectLst/></a:effectStyle>".repeat(3);
    format!(
        r#"{XML_HEAD}<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Harness"><a:themeElements><a:clrScheme name="Harness"><a:dk1><a:srgbClr val="{}"/></a:dk1><a:lt1><a:srgbClr val="{}"/></a:lt1><a:dk2><a:srgbClr val="{}"/></a:dk2><a:lt2><a:srgbClr val="F3F4F6"/></a:lt2><a:accent1><a:srgbClr val="{}"/></a:accent1><a:accent2><a:srgbClr val="F59E0B"/></a:accent2><a:accent3><a:srgbClr val="10B981"/></a:accent3><a:accent4><a:srgbClr val="EF4444"/></a:accent4><a:accent5><a:srgbClr val="8B5CF6"/></a:accent5><a:accent6><a:srgbClr val="06B6D4"/></a:accent6><a:hlink><a:srgbClr val="{}"/></a:hlink><a:folHlink><a:srgbClr val="8B5CF6"/></a:folHlink></a:clrScheme><a:fontScheme name="Harness"><a:majorFont><a:latin typeface="{head}"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="{font}"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Harness"><a:fillStyleLst>{fills}</a:fillStyleLst><a:lnStyleLst>{lines}</a:lnStyleLst><a:effectStyleLst>{effects}</a:effectStyleLst><a:bgFillStyleLst>{fills}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
        c(&t.foreground, "1F2328"),
        c(&t.background, "FFFFFF"),
        c(&t.muted, "6B7280"),
        c(&t.accent, "3B6CF6"),
        c(&t.accent, "3B6CF6"),
    )
}
