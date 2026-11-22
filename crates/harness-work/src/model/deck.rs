//! `deck.json` (`harness-deck/1`): theme plus slides. Every slide picks a layout; the layout decides
//! which fields are used. Free slides place their own elements on a 1280×720 pt canvas.

use serde::{Deserialize, Serialize};

use super::chart::Chart;

pub const FORMAT: &str = "harness-deck/1";
pub const WIDTH: f64 = 1280.0;
pub const HEIGHT: f64 = 720.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    #[serde(default = "Theme::bg")]
    pub background: String,
    #[serde(default = "Theme::fg")]
    pub foreground: String,
    #[serde(default = "Theme::accent")]
    pub accent: String,
    #[serde(default = "Theme::muted")]
    pub muted: String,
    #[serde(default = "Theme::font")]
    pub font: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_font: Option<String>,
    /// Footer text on every slide but the title slide (brand kit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<String>,
    /// Logo image relative to the item folder, drawn top right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
}

impl Theme {
    fn bg() -> String {
        "#ffffff".into()
    }
    fn fg() -> String {
        "#1f2328".into()
    }
    fn accent() -> String {
        "#3b6cf6".into()
    }
    fn muted() -> String {
        "#6b7280".into()
    }
    fn font() -> String {
        "Segoe UI".into()
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Self::bg(),
            foreground: Self::fg(),
            accent: Self::accent(),
            muted: Self::muted(),
            font: Self::font(),
            heading_font: None,
            footer: None,
            logo: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    Title,
    Section,
    #[default]
    Bullets,
    TwoColumn,
    Image,
    ImageText,
    Table,
    Chart,
    Quote,
    BigNumber,
    Free,
}

impl Layout {
    pub const ALL: [Layout; 11] = [
        Self::Title,
        Self::Section,
        Self::Bullets,
        Self::TwoColumn,
        Self::Image,
        Self::ImageText,
        Self::Table,
        Self::Chart,
        Self::Quote,
        Self::BigNumber,
        Self::Free,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Section => "section",
            Self::Bullets => "bullets",
            Self::TwoColumn => "two-column",
            Self::Image => "image",
            Self::ImageText => "image-text",
            Self::Table => "table",
            Self::Chart => "chart",
            Self::Quote => "quote",
            Self::BigNumber => "big-number",
            Self::Free => "free",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TableData {
    #[serde(default)]
    pub header: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<String>>,
}

impl TableData {
    #[must_use]
    pub fn columns(&self) -> usize {
        self.rows
            .iter()
            .map(Vec::len)
            .chain(std::iter::once(self.header.len()))
            .max()
            .unwrap_or(0)
    }
}

/// An element of a free slide. Positions are points on the 1280×720 canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Element {
    pub id: String,
    /// `text`, `image`, `shape`, `table` or `chart`.
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<TableData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<Chart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Slide {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bullets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub right: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caption: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<TableData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chart: Option<Chart>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub quote: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub number: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Element>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deck {
    pub format: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub slides: Vec<Slide>,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            format: FORMAT.into(),
            title: String::new(),
            theme: Theme::default(),
            slides: Vec::new(),
        }
    }
}

impl Deck {
    /// Gives every slide without an id a stable one (`s1`, `s2`, …) so comments can point at it.
    pub fn fill_ids(&mut self) {
        let mut used: std::collections::HashSet<String> =
            self.slides.iter().map(|s| s.id.clone()).collect();
        let mut n = 1;
        for slide in &mut self.slides {
            if slide.id.is_empty() {
                while used.contains(&format!("s{n}")) {
                    n += 1;
                }
                slide.id = format!("s{n}");
                used.insert(slide.id.clone());
            }
        }
    }
}
