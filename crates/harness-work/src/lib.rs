//! Harness Work items (spec `docs/specs/harness/work-edition.md`).
//!
//! A Work item is a folder `work/<slug>/` holding one source file (`deck.json`, `book.json`,
//! `doc.md` or `chart.json`) plus `assets/`, `exports/` and `.revisions/`. This crate owns the
//! source formats, validation with element paths, the Typst renderer (preview PNG and PDF), the
//! `IronCalc` sheet engine, the Office writers and readers, the folder scanner and the
//! `harness-work` MCP server that agents call.
//!
//! Everything here is blocking; async callers wrap it in `spawn_blocking`.

#![allow(clippy::missing_errors_doc)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod brand;
mod create;
mod error;
mod export;
mod import;
mod item;
mod mcp;
mod model;
mod path;
mod render;
mod scan;
mod sheet;
mod validate;

pub use brand::{load_brand, save_brand, Brand};
pub use create::{create, save_template, slugify, templates, TemplateInfo};
pub use error::{WorkError, WorkResult};
pub use export::{export, export_clean, ExportFormat};
pub use import::{import, ImportReport};
pub use ironcalc::base::cell::CellValue;
pub use item::{Kind, Source, WorkItem};
pub use mcp::run as run_mcp;
pub use model::book::{column_name, parse_cell, Book, Sheet};
pub use model::chart::{Chart, ChartType, Series};
pub use model::deck::{Deck, Element, Layout, Slide, TableData, Theme};
pub use model::doc::{Doc, DocMeta};
pub use model::text::{runs, Run};
pub use path::ElementPath;
pub use render::{render, render_pdf, Hit, Page, Rendered, Target};
pub use scan::{scan, ItemSummary};
pub use sheet::{SheetGrid, SheetModel};
pub use validate::{validate, Issue, Severity};

/// Folder under the project root that holds every Work item.
pub const WORK_DIR: &str = "work";
