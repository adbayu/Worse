//! Exports into `<item>/exports/`: pptx (native shapes and charts), docx, pdf (Typst), png pages,
//! html (self-contained), xlsx/csv (`IronCalc`) and svg charts.

mod chart_xml;
mod docx;
mod html;
mod ooxml;
mod pptx;
pub(crate) mod pptx_shapes;
mod pptx_slides;
mod xlsx_parts;
mod xlsx_patch;
mod xlsx_style;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{write_file, WorkError, WorkResult};
use crate::item::{Kind, Source, WorkItem};
use crate::render::{render, render_pdf, Target};
use crate::sheet::SheetModel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Pptx,
    Docx,
    Xlsx,
    Csv,
    Pdf,
    Png,
    Html,
    Svg,
}

impl ExportFormat {
    pub const ALL: [Self; 8] = [
        Self::Pptx,
        Self::Docx,
        Self::Xlsx,
        Self::Csv,
        Self::Pdf,
        Self::Png,
        Self::Html,
        Self::Svg,
    ];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let n = name.trim().trim_start_matches('.').to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|f| f.ext() == n)
            .or(match n.as_str() {
                "powerpoint" | "slides" => Some(Self::Pptx),
                "word" => Some(Self::Docx),
                "excel" => Some(Self::Xlsx),
                "htm" => Some(Self::Html),
                _ => None,
            })
    }

    #[must_use]
    pub fn ext(self) -> &'static str {
        match self {
            Self::Pptx => "pptx",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Csv => "csv",
            Self::Pdf => "pdf",
            Self::Png => "png",
            Self::Html => "html",
            Self::Svg => "svg",
        }
    }

    /// Formats offered for a kind (first = default).
    #[must_use]
    pub fn for_kind(kind: Kind) -> &'static [Self] {
        match kind {
            Kind::Deck => &[Self::Pptx, Self::Pdf, Self::Png, Self::Html],
            Kind::Doc => &[Self::Docx, Self::Pdf, Self::Html, Self::Png],
            Kind::Book => &[Self::Xlsx, Self::Csv, Self::Pdf, Self::Html],
            Kind::Chart => &[Self::Png, Self::Svg, Self::Pdf, Self::Html],
        }
    }
}

fn brand_accent(item: &WorkItem) -> String {
    item.dir
        .parent()
        .map_or_else(|| "#3b6cf6".into(), |p| crate::brand::load_brand(p).accent)
}

/// Writes the export and returns the files written (several for png pages and csv sheets).
/// For imported `.xlsx` workbooks, patches the original file in place so charts, pivots, macros
/// and styles survive; use [`export_clean`] to rebuild from `IronCalc` instead.
pub fn export(item: &WorkItem, format: ExportFormat) -> WorkResult<Vec<PathBuf>> {
    export_impl(item, format, false)
}

/// Like [`export`], but for `.xlsx` always writes a clean file via `IronCalc` instead of patching
/// the imported original.
pub fn export_clean(item: &WorkItem, format: ExportFormat) -> WorkResult<Vec<PathBuf>> {
    export_impl(item, format, true)
}

fn export_impl(item: &WorkItem, format: ExportFormat, clean: bool) -> WorkResult<Vec<PathBuf>> {
    if !ExportFormat::for_kind(item.kind).contains(&format) {
        return Err(WorkError::Invalid(format!(
            "{} items cannot be exported as {}",
            item.kind.label(),
            format.ext()
        )));
    }
    let source = item.load()?;
    let dir = item.exports_dir();
    let slug = item.slug();
    let one = |bytes: &[u8]| -> WorkResult<Vec<PathBuf>> {
        let path = dir.join(format!("{slug}.{}", format.ext()));
        write_file(&path, bytes)?;
        Ok(vec![path])
    };
    match (format, &source) {
        (ExportFormat::Pptx, Source::Deck(d)) => one(&pptx::pptx(item, d)?),
        (ExportFormat::Docx, Source::Doc(d)) => one(&docx::docx(item, d, &brand_accent(item))?),
        (ExportFormat::Pdf, _) => one(&render_pdf(item)?),
        (ExportFormat::Html, _) => one(html::html(item, &source)?.as_bytes()),
        (ExportFormat::Svg, Source::Chart(c)) => {
            let resolved = crate::render::data::resolve_chart(item, c);
            one(crate::render::chart_svg::svg(
                &resolved,
                800.0,
                450.0,
                &crate::render::chart_svg::Style::default(),
            )
            .as_bytes())
        }
        (ExportFormat::Png, _) => {
            let pages = render(item, &Target::All, 2.0)?;
            let many = pages.pages.len() > 1;
            let mut out = Vec::new();
            for p in &pages.pages {
                let name = if many {
                    format!("{slug}-{}.png", p.index + 1)
                } else {
                    format!("{slug}.png")
                };
                let path = dir.join(name);
                write_file(&path, &p.png)?;
                out.push(path);
            }
            Ok(out)
        }
        (ExportFormat::Xlsx, Source::Book(b)) => {
            let path = dir.join(format!("{slug}.xlsx"));
            std::fs::create_dir_all(&dir).map_err(|e| WorkError::io(&dir, e))?;
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| WorkError::io(&path, e))?;
            }
            let model = SheetModel::from_book(b)?;
            // 写回失败时不再悄悄退回干净导出（会丢掉图表、透视表和宏），直接报错让用户选择。
            if let Some(orig) = crate::import::original_file(item).filter(|_| !clean) {
                let bytes = std::fs::read(&orig).map_err(|e| WorkError::io(&orig, e))?;
                let patched = xlsx_patch::patch(&bytes, b, &model).map_err(|e| {
                    WorkError::Invalid(format!(
                        "{e}. Export with clean: true to rebuild the file without the original's charts, pivots and macros."
                    ))
                })?;
                // .xlsm 原件带宏，扩展名必须一致，否则 Excel 拒绝打开。
                let ext = orig
                    .extension()
                    .and_then(|e| e.to_str())
                    .map_or_else(|| "xlsx".to_string(), str::to_ascii_lowercase);
                let out = dir.join(format!("{slug}.{ext}"));
                write_file(&out, &patched)?;
                return Ok(vec![out]);
            }
            model.save_xlsx(&path)?;
            Ok(vec![path])
        }
        (ExportFormat::Csv, Source::Book(b)) => {
            let model = SheetModel::from_book(b)?;
            let mut out = Vec::new();
            for (i, sheet) in b.sheets.iter().enumerate() {
                let Ok(idx) = u32::try_from(i) else { continue };
                let name = if b.sheets.len() > 1 {
                    format!("{slug}-{}.csv", crate::create::slugify(&sheet.name))
                } else {
                    format!("{slug}.csv")
                };
                let path = dir.join(name);
                write_file(&path, model.csv(idx).as_bytes())?;
                out.push(path);
            }
            Ok(out)
        }
        _ => Err(WorkError::Invalid(format!(
            "{} items cannot be exported as {}",
            item.kind.label(),
            format.ext()
        ))),
    }
}

#[cfg(test)]
mod tests;
