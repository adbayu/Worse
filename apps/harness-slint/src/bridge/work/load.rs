//! Blocking worker operations for Work items: scanning `work/*/`, Typst rendering into RGBA
//! pixel buffers for Slint, `IronCalc` sheet grids, and validation summaries.

use std::collections::HashMap;
use std::path::Path;

use harness_work::{validate, ItemSummary, Kind, Severity, SheetModel, Source, Target, WorkItem};
use slint::{Rgba8Pixel, SharedPixelBuffer};

use super::grid::LoadedRow;

const MAX_SHEET_ROWS: i32 = 80;
const MAX_SHEET_COLS: i32 = 20;

pub(super) struct RenderedSlide {
    pub index: usize,
    pub title: String,
    pub notes: String,
    pub preview: Option<SharedPixelBuffer<Rgba8Pixel>>,
    pub thumb: Option<SharedPixelBuffer<Rgba8Pixel>>,
}

pub(super) struct LoadedWork {
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub warning: String,
    pub slides: Vec<RenderedSlide>,
    pub sheet_names: Vec<String>,
    /// Sheets hidden in Excel: no tab, still evaluated.
    pub sheet_hidden: Vec<bool>,
    pub active_sheet: usize,
    /// Column letters with their width in px.
    pub col_headers: Vec<(String, f32)>,
    pub sheet_rows: Vec<LoadedRow>,
    pub sheet_notes: HashMap<String, String>,
    pub sheet_validations: Vec<(String, Vec<String>)>,
}

pub(super) fn kind_str(kind: Kind) -> &'static str {
    match kind {
        Kind::Deck => "deck",
        Kind::Book => "sheet",
        Kind::Doc => "doc",
        Kind::Chart => "chart",
    }
}

pub(super) fn summary_detail(item: &ItemSummary) -> String {
    if item.broken {
        return format!("{} · parse error", item.kind.label());
    }
    let noun = match item.kind {
        Kind::Deck => {
            if item.parts == 1 {
                "slide"
            } else {
                "slides"
            }
        }
        Kind::Book => {
            if item.parts == 1 {
                "sheet"
            } else {
                "sheets"
            }
        }
        Kind::Doc => {
            if item.parts == 1 {
                "section"
            } else {
                "sections"
            }
        }
        Kind::Chart => "chart",
    };
    if item.kind == Kind::Chart {
        "Chart".into()
    } else {
        format!("{} · {} {noun}", item.kind.label(), item.parts)
    }
}

pub(super) fn scan_items(root: &Path) -> Vec<ItemSummary> {
    harness_work::scan(root)
}

fn decode_png(bytes: &[u8], max_width: Option<u32>) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = match max_width {
        Some(w) if img.width() > w => img.thumbnail(w, w).to_rgba8(),
        _ => img.to_rgba8(),
    };
    Some(SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

pub(super) use harness_work::column_name as col_name;

/// Column letters with their width in px.
type ColHeaders = Vec<(String, f32)>;

struct LoadedSheet {
    names: Vec<String>,
    active: usize,
    col_headers: ColHeaders,
    rows: Vec<LoadedRow>,
    notes: HashMap<String, String>,
    validations: Vec<(String, Vec<String>)>,
    status: String,
}

fn load_sheet(book: &harness_work::Book, sheet_idx: usize) -> LoadedSheet {
    let names: Vec<String> = book.sheets.iter().map(|s| s.name.clone()).collect();
    let mut active = sheet_idx.min(names.len().saturating_sub(1));
    if book.sheets.get(active).is_some_and(|s| s.hidden) {
        active = book.sheets.iter().position(|s| !s.hidden).unwrap_or(active);
    }
    let Ok(model) = SheetModel::from_book(book) else {
        return LoadedSheet {
            names,
            active,
            col_headers: Vec::new(),
            rows: Vec::new(),
            notes: HashMap::new(),
            validations: Vec::new(),
            status: "Failed to evaluate sheet".into(),
        };
    };
    let idx = u32::try_from(active).unwrap_or(0);
    let (used_r, used_c) = model.used(idx);
    let rows = used_r.clamp(12, MAX_SHEET_ROWS);
    let cols = used_c.clamp(6, MAX_SHEET_COLS);
    let grid = model.grid(idx, 1, 1, rows, cols);
    let (vis_cols, loaded_rows) = super::grid::build(&grid, book.sheets.get(active));
    let col_headers: ColHeaders = vis_cols
        .into_iter()
        .map(|(c, w)| (col_name(c), w))
        .collect();
    let mut notes = HashMap::new();
    for row in &loaded_rows {
        for cell in &row.cells {
            if !cell.note.is_empty() {
                notes.insert(cell.coord.clone(), cell.note.clone());
            } else if cell.error {
                let msg = if cell.formula.is_empty() {
                    format!("Cell error: {}", cell.display)
                } else {
                    format!("Formula error {}: {}", cell.display, cell.formula)
                };
                notes.insert(cell.coord.clone(), msg);
            }
        }
    }
    let mut validations = Vec::new();
    if let Some(sh) = book.sheets.get(active) {
        for (range, items) in &sh.validation {
            let resolved: Vec<String> = if items.len() == 1 && items[0].starts_with('=') {
                model
                    .range(&items[0][1..])
                    .ok()
                    .map(|rows| {
                        rows.into_iter()
                            .flatten()
                            .filter_map(|v| match v {
                                harness_work::CellValue::String(s) if !s.is_empty() => Some(s),
                                harness_work::CellValue::Number(n) => Some(n.to_string()),
                                _ => None,
                            })
                            .take(24)
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                items.iter().take(24).cloned().collect()
            };
            if !resolved.is_empty() {
                validations.push((range.clone(), resolved));
            }
        }
    }
    // 网格最多显示 MAX_SHEET_ROWS×MAX_SHEET_COLS，超出时在状态栏注明，避免用户以为数据缺失。
    let clipped = if used_r > rows || used_c > cols {
        format!(" · showing {rows}×{cols}")
    } else {
        String::new()
    };
    let status = format!("{} sheets · {used_r}×{used_c} used{clipped}", names.len());
    LoadedSheet {
        names,
        active,
        col_headers,
        rows: loaded_rows,
        notes,
        validations,
        status,
    }
}

pub(super) fn load_item(root: &Path, slug: &str, sheet_idx: usize) -> Result<LoadedWork, String> {
    let dir = root.join(harness_work::WORK_DIR).join(slug);
    let item = WorkItem::open(&dir).map_err(|e| e.to_string())?;
    let source = item.load().map_err(|e| e.to_string())?;
    let title = item.title_of(&source);
    let kind = kind_str(item.kind).to_string();

    let issues = validate(&item);
    let err_count = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    let warn_count = issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .count();
    let mut warning = if err_count > 0 {
        issues
            .iter()
            .find(|i| i.severity == Severity::Error)
            .map(|i| format!("{err_count} error(s): {}", i.message))
            .unwrap_or_default()
    } else if warn_count > 0 {
        issues
            .iter()
            .find(|i| i.severity == Severity::Warning)
            .map(|i| format!("{warn_count} warning(s): {}", i.message))
            .unwrap_or_default()
    } else {
        String::new()
    };

    if let Source::Book(ref book) = source {
        let s = load_sheet(book, sheet_idx);
        return Ok(LoadedWork {
            slug: slug.to_string(),
            title,
            kind,
            status: s.status,
            warning,
            slides: Vec::new(),
            sheet_hidden: book.sheets.iter().map(|sh| sh.hidden).collect(),
            sheet_names: s.names,
            active_sheet: s.active,
            col_headers: s.col_headers,
            sheet_rows: s.rows,
            sheet_notes: s.notes,
            sheet_validations: s.validations,
        });
    }

    let rendered = harness_work::render(&item, &Target::All, 1.0).map_err(|e| e.to_string())?;
    if warning.is_empty() && !rendered.warnings.is_empty() {
        warning.clone_from(&rendered.warnings[0]);
    }
    let mut slides = Vec::with_capacity(rendered.pages.len());
    for page in &rendered.pages {
        let (slide_title, notes) = match &source {
            Source::Deck(d) => d.slides.get(page.index).map_or_else(
                || (format!("Slide {}", page.index + 1), String::new()),
                |s| {
                    let t = if s.title.is_empty() {
                        format!("Slide {}", page.index + 1)
                    } else {
                        s.title.clone()
                    };
                    (t, s.notes.clone())
                },
            ),
            Source::Doc(_) => (format!("Page {}", page.index + 1), String::new()),
            Source::Chart(_) | Source::Book(_) => (title.clone(), String::new()),
        };
        let preview = decode_png(&page.png, None);
        let thumb = decode_png(&page.png, Some(180));
        slides.push(RenderedSlide {
            index: page.index,
            title: slide_title,
            notes,
            preview,
            thumb,
        });
    }
    let noun = match item.kind {
        Kind::Deck => "slides",
        Kind::Doc => "pages",
        Kind::Chart => "chart",
        Kind::Book => "sheets",
    };
    let status = format!("{} {noun}", slides.len());
    Ok(LoadedWork {
        slug: slug.to_string(),
        title,
        kind,
        status,
        warning,
        slides,
        sheet_names: Vec::new(),
        sheet_hidden: Vec::new(),
        active_sheet: 0,
        col_headers: Vec::new(),
        sheet_rows: Vec::new(),
        sheet_notes: HashMap::new(),
        sheet_validations: Vec::new(),
    })
}
