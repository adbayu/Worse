//! Brand kit `work/brand.json`: colors, fonts, logo and footer applied to new items (deck theme, doc
//! accent) and to exports.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{write_file, WorkResult};
use crate::item::to_json;
use crate::model::deck::Theme;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Brand {
    pub name: String,
    pub accent: String,
    pub background: String,
    pub foreground: String,
    pub muted: String,
    pub font: String,
    pub heading_font: String,
    /// Logo file relative to the `work/` folder.
    pub logo: String,
    pub footer: String,
}

impl Default for Brand {
    fn default() -> Self {
        let t = Theme::default();
        Self {
            name: String::new(),
            accent: t.accent,
            background: t.background,
            foreground: t.foreground,
            muted: t.muted,
            font: t.font,
            heading_font: String::new(),
            logo: String::new(),
            footer: String::new(),
        }
    }
}

impl Brand {
    /// Deck theme from the brand (the logo path is set by `create` after copying the file).
    #[must_use]
    pub fn theme(&self) -> Theme {
        Theme {
            background: self.background.clone(),
            foreground: self.foreground.clone(),
            accent: self.accent.clone(),
            muted: self.muted.clone(),
            font: self.font.clone(),
            heading_font: (!self.heading_font.is_empty()).then(|| self.heading_font.clone()),
            footer: (!self.footer.is_empty()).then(|| self.footer.clone()),
            logo: None,
        }
    }
}

/// Loads `<work_dir>/brand.json` (defaults when missing or broken).
#[must_use]
pub fn load_brand(work_dir: &Path) -> Brand {
    std::fs::read_to_string(work_dir.join("brand.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_brand(work_dir: &Path, brand: &Brand) -> WorkResult<()> {
    write_file(&work_dir.join("brand.json"), to_json(brand)?.as_bytes())
}
