//! Typst rendering: source → Typst markup → pages (PNG) with hit boxes, or PDF. Fonts are loaded
//! once; compiles are memoized by Typst; rendered pages are cached by source hash + target.

pub(crate) mod chart_svg;
mod chart_svg_axes;
pub(crate) mod data;
pub(crate) mod deck;
pub(crate) mod doc;
pub(crate) mod markup;
mod world;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use sha2::{Digest, Sha256};
use typst::foundations::{Label, Selector};
use typst::introspection::{Introspector as _, MetadataElem};
use typst::utils::PicoStr;
use typst_layout::PagedDocument;

pub use world::warm_up;

use crate::error::{WorkError, WorkResult};
use crate::item::{Source, WorkItem};
use crate::model::deck::Deck;

/// Bump when templates change so cached pages are dropped.
const TEMPLATE_VERSION: u32 = 1;
const CACHE_MAX: usize = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    All,
    /// Zero-based slide or page.
    Page(usize),
    /// A sheet range (`sheet` empty = first sheet).
    Range {
        sheet: String,
        range: String,
    },
}

impl Target {
    /// `all`, `slide 3` / `page 3` (one-based), `Sheet1!A1:F20`.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("all") {
            return Self::All;
        }
        let digits: String = t.chars().filter(char::is_ascii_digit).collect();
        if !t.contains(':') && !t.contains('!') && !digits.is_empty() {
            return Self::Page(digits.parse::<usize>().unwrap_or(1).saturating_sub(1));
        }
        match t.rsplit_once('!') {
            Some((s, r)) => Self::Range {
                sheet: s.trim_matches('\'').to_string(),
                range: r.to_string(),
            },
            None => Self::Range {
                sheet: String::new(),
                range: t.to_string(),
            },
        }
    }
}

/// A hit box in points on its page (`page` zero-based).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    pub path: String,
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub overflow: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Zero-based index in the whole item.
    pub index: usize,
    pub png: Vec<u8>,
    pub width_px: u32,
    pub height_px: u32,
    pub width_pt: f64,
    pub height_pt: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rendered {
    pub pages: Vec<Page>,
    pub hits: Vec<Hit>,
    /// Pages (slides) in the whole item.
    pub page_count: usize,
    /// Typst warnings plus overflow warnings with element paths.
    pub warnings: Vec<String>,
}

type PageCache = Mutex<(Vec<String>, HashMap<String, Rendered>)>;

fn cache() -> &'static PageCache {
    static CACHE: OnceLock<PageCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new((Vec::new(), HashMap::new())))
}

fn key(item: &WorkItem, raw: &str, target: &Target, scale: f32) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    h.update(item.dir.to_string_lossy().as_bytes());
    h.update(format!("{target:?}|{scale}|{TEMPLATE_VERSION}").as_bytes());
    // 资产文件变化也要让缓存失效：只用目录下文件的修改时间。
    if let Ok(entries) = std::fs::read_dir(item.dir.join("assets")) {
        for e in entries.flatten() {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                h.update(format!("{:?}{m:?}", e.file_name()).as_bytes());
            }
        }
    }
    format!("{:x}", h.finalize())
}

/// Renders an item to PNG pages at `scale` pixels per point.
pub fn render(item: &WorkItem, target: &Target, scale: f32) -> WorkResult<Rendered> {
    let raw = item.read_raw()?;
    let k = key(item, &raw, target, scale);
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.1.get(&k) {
            return Ok(hit.clone());
        }
    }
    let source = WorkItem::parse(item.kind, &raw).map_err(WorkError::Invalid)?;
    let (built, page_count, offset) = build(item, &source, target);
    let doc = compile(item, built)?;
    let pages = doc.0.pages();
    let total = page_count.unwrap_or(pages.len());
    let opts = typst_render::RenderOptions {
        pixel_per_pt: typst::utils::Scalar::new(f64::from(scale.clamp(0.1, 4.0))),
        render_bleed: false,
    };
    let mut out = Rendered {
        page_count: total,
        warnings: doc.1,
        ..Rendered::default()
    };
    for (i, page) in pages.iter().enumerate() {
        if offset.is_none() && matches!(target, Target::Page(p) if *p != i) {
            continue;
        }
        let pix = typst_render::render(page, &opts);
        let size = page.frame.size();
        out.pages.push(Page {
            index: offset.unwrap_or(i),
            width_px: pix.width(),
            height_px: pix.height(),
            png: pix
                .encode_png()
                .map_err(|e| WorkError::Render(e.to_string()))?,
            width_pt: size.x.to_pt(),
            height_pt: size.y.to_pt(),
        });
    }
    out.hits = hits(&doc.0, offset);
    for h in out.hits.iter().filter(|h| h.overflow) {
        out.warnings.push(format!(
            "{}: text overflows its box; shorten it or split the slide",
            h.path
        ));
    }
    if let Ok(mut c) = cache().lock() {
        if c.0.len() >= CACHE_MAX {
            let old = c.0.remove(0);
            c.1.remove(&old);
        }
        c.0.push(k.clone());
        c.1.insert(k, out.clone());
    }
    Ok(out)
}

/// The whole item as PDF.
pub fn render_pdf(item: &WorkItem) -> WorkResult<Vec<u8>> {
    let source = item.load()?;
    let (built, _, _) = build(item, &source, &Target::All);
    let (doc, _) = compile(item, built)?;
    typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).map_err(|e| WorkError::Render(diag(&e)))
}

/// Returns the Typst build, the total page count when known up front (decks), and the slide offset
/// when only one slide was built.
fn build(
    item: &WorkItem,
    source: &Source,
    target: &Target,
) -> (deck::Built, Option<usize>, Option<usize>) {
    match source {
        Source::Deck(d) => {
            let only = match target {
                Target::Page(p) => Some(*p),
                _ => None,
            };
            (deck::build(item, d, only), Some(d.slides.len()), only)
        }
        Source::Doc(d) => (doc::build_doc(item, d, &brand_accent(item)), None, None),
        Source::Book(b) => {
            let only = match target {
                Target::Range { sheet, range } => Some((sheet.as_str(), range.as_str())),
                _ => None,
            };
            (doc::build_book(item, b, only), None, None)
        }
        Source::Chart(c) => (doc::build_chart(item, c), None, None),
    }
}

fn brand_accent(item: &WorkItem) -> String {
    item.dir.parent().map_or_else(
        || Deck::default().theme.accent,
        |p| crate::brand::load_brand(p).accent,
    )
}

fn diag(errors: &[typst::diag::SourceDiagnostic]) -> String {
    errors
        .iter()
        .map(|d| d.message.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn compile(item: &WorkItem, built: deck::Built) -> WorkResult<(PagedDocument, Vec<String>)> {
    let world = world::WorkWorld::new(built.source, item.dir.clone(), built.files);
    let result = typst::compile::<PagedDocument>(&world);
    let warnings = result
        .warnings
        .iter()
        .map(|w| w.message.to_string())
        .collect();
    let doc = result.output.map_err(|e| WorkError::Render(diag(&e)))?;
    Ok((doc, warnings))
}

/// Rasterizes an SVG (`w`×`h` pt) to PNG through Typst (charts in docx exports).
pub(crate) fn svg_png(svg: Vec<u8>, w: f64, h: f64, scale: f32) -> WorkResult<Vec<u8>> {
    let mut files = std::collections::HashMap::new();
    files.insert("__hw/x.svg".to_string(), svg);
    let source = format!("#set page(width: {w}pt, height: {h}pt, margin: 0pt)\n#image(\"/__hw/x.svg\", width: 100%, height: 100%)\n");
    let world = world::WorkWorld::new(source, std::env::temp_dir(), files);
    let doc = typst::compile::<PagedDocument>(&world)
        .output
        .map_err(|e| WorkError::Render(diag(&e)))?;
    let page = doc
        .pages()
        .first()
        .ok_or_else(|| WorkError::Render("empty page".into()))?;
    let opts = typst_render::RenderOptions {
        pixel_per_pt: typst::utils::Scalar::new(f64::from(scale)),
        render_bleed: false,
    };
    typst_render::render(page, &opts)
        .encode_png()
        .map_err(|e| WorkError::Render(e.to_string()))
}

fn num(v: &serde_json::Value, k: &str) -> f64 {
    v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0)
}

fn hits(doc: &PagedDocument, offset: Option<usize>) -> Vec<Hit> {
    let Some(label) = Label::new(PicoStr::intern("hw-hit")) else {
        return Vec::new();
    };
    let mut out: Vec<Hit> = Vec::new();
    for c in &doc.introspector().query(&Selector::Label(label)) {
        let Some(m) = c.to_packed::<MetadataElem>() else {
            continue;
        };
        let Ok(v) = serde_json::to_value(&m.value) else {
            continue;
        };
        let page = usize::try_from(
            v.get("page")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1),
        )
        .unwrap_or(1)
        .saturating_sub(1);
        out.push(Hit {
            path: v
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            page: offset.unwrap_or(page),
            x: num(&v, "x"),
            y: num(&v, "y"),
            w: num(&v, "w"),
            h: num(&v, "h"),
            overflow: v
                .get("over")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    // 文档标记只有起点：宽度取版心，高度到下一个标记或页底。
    let sizes: Vec<(f64, f64)> = doc
        .pages()
        .iter()
        .map(|p| (p.frame.size().x.to_pt(), p.frame.size().y.to_pt()))
        .collect();
    for i in 0..out.len() {
        if out[i].w > 0.0 {
            continue;
        }
        let (pw, ph) = sizes.get(out[i].page).copied().unwrap_or((595.0, 842.0));
        let next = out
            .iter()
            .skip(i + 1)
            .find(|n| n.page == out[i].page && n.y > out[i].y + 0.5)
            .map(|n| n.y);
        let hit = &mut out[i];
        hit.x = hit.x.min(pw / 2.0);
        hit.w = (pw - 2.0 * hit.x).max(10.0);
        hit.h = next.unwrap_or(ph - hit.x).max(hit.y + 12.0) - hit.y;
    }
    out
}

#[cfg(test)]
mod tests;
