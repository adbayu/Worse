//! Imports Office files into new Work items: `.xlsx`/`.csv` → book, `.pptx` → deck, `.docx`/`.md` →
//! doc. The original file is kept in `assets/` so nothing is lost when the mapping is partial.

mod audit;
mod docx;
mod pptx;
pub(crate) mod sheet;
mod table_refs;
pub(crate) mod xlsx_extra;
pub(crate) mod xml;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::create::slugify;
use crate::error::{write_file, WorkError, WorkResult};
use crate::item::{to_json, Kind, WorkItem};
use crate::WORK_DIR;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub dir: PathBuf,
    pub kind: Kind,
    pub title: String,
    /// What could not be carried over (animations, merged layouts, macros, …).
    pub warnings: Vec<String>,
}

/// What an importer produced: source text, files for `assets/`, warnings.
pub(crate) struct Imported {
    pub kind: Kind,
    pub source: String,
    pub assets: Vec<(String, Vec<u8>)>,
    pub warnings: Vec<String>,
}

fn new_dir(project: &Path, title: &str) -> PathBuf {
    let work = project.join(WORK_DIR);
    let slug = slugify(title);
    let mut dir = work.join(&slug);
    let mut n = 2;
    while dir.exists() {
        dir = work.join(format!("{slug}-{n}"));
        n += 1;
    }
    dir
}

/// Imports `file` into `<project>/work/<slug>/`.
pub fn import(project: &Path, file: &Path) -> WorkResult<ImportReport> {
    let ext = file
        .extension()
        .map_or_else(String::new, |e| e.to_string_lossy().to_ascii_lowercase());
    let bytes = std::fs::read(file).map_err(|e| WorkError::io(file, e))?;
    let stem = file
        .file_stem()
        .map_or_else(|| "imported".into(), |s| s.to_string_lossy().into_owned());
    let imported = match ext.as_str() {
        "xlsx" | "xlsm" => sheet::xlsx(&bytes, &stem)?,
        "csv" | "tsv" => sheet::csv(
            &String::from_utf8_lossy(&bytes),
            &stem,
            if ext == "tsv" { '\t' } else { ',' },
        )?,
        "pptx" => pptx::pptx(&bytes, &stem)?,
        "docx" => docx::docx(&bytes, &stem)?,
        "md" | "markdown" | "txt" => Imported {
            kind: Kind::Doc,
            source: String::from_utf8_lossy(&bytes).into_owned(),
            assets: Vec::new(),
            warnings: Vec::new(),
        },
        other => {
            return Err(WorkError::Import(format!(
                "unsupported file type .{other} (use xlsx, csv, pptx, docx or md)"
            )))
        }
    };
    let parsed = WorkItem::parse(imported.kind, &imported.source).map_err(WorkError::Import)?;
    let dir = new_dir(project, &stem);
    let item = WorkItem {
        dir: dir.clone(),
        kind: imported.kind,
    };
    let title = {
        let t = item.title_of(&parsed);
        if t.is_empty() {
            stem.clone()
        } else {
            t
        }
    };
    std::fs::create_dir_all(dir.join("assets")).map_err(|e| WorkError::io(&dir, e))?;
    for (name, data) in &imported.assets {
        let Some(path) = item.asset(name) else {
            continue;
        };
        write_file(&path, data)?;
    }
    if let Some(name) = file.file_name() {
        write_file(&dir.join("assets").join("original").join(name), &bytes)?;
    }
    if matches!(ext.as_str(), "xlsx" | "xlsm") {
        // 导入的工作簿把样式放进 style.json，book.json 只留数据，方便智能体读取。
        write_file(&dir.join(crate::model::look::FILE), b"{}")?;
    }
    // JSON 来源统一重新序列化，保证格式整齐、ID 完整。
    match parsed {
        crate::item::Source::Doc(_) => write_file(&item.source_path(), imported.source.as_bytes())?,
        crate::item::Source::Deck(d) => write_file(&item.source_path(), to_json(&d)?.as_bytes())?,
        other => item.save(&other)?,
    }
    Ok(ImportReport {
        dir,
        kind: imported.kind,
        title,
        warnings: imported.warnings,
    })
}

/// The imported original `.xlsx` of a sheet item as a book (`None` when there is none).
pub(crate) fn original_file(item: &WorkItem) -> Option<PathBuf> {
    std::fs::read_dir(item.dir.join("assets").join("original"))
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xlsx") || e.eq_ignore_ascii_case("xlsm"))
        })
}

pub(crate) fn original(item: &WorkItem) -> Option<crate::item::Source> {
    let path = original_file(item)?;
    let bytes = std::fs::read(&path).ok()?;
    let imported = sheet::xlsx_plain(&bytes, "original").ok()?;
    WorkItem::parse(Kind::Book, &imported.source).ok()
}

#[cfg(test)]
mod tests;
