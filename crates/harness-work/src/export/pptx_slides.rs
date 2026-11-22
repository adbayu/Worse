//! Slide layout rendering for `.pptx` export (companion to `pptx.rs`).

use super::pptx::{Out, BODY, LEFT, RIGHT};
use super::pptx_shapes::{para, shape, table, Font};
use crate::model::deck::{Element, Layout, Slide};

impl Out<'_> {
    pub(super) fn element(&mut self, e: &Element) {
        let r = (e.x, e.y, e.w, e.h);
        match e.kind.as_str() {
            "image" => self.image(r, e.image.as_deref().unwrap_or_default()),
            "table" => {
                if let Some(t) = &e.table {
                    let id = self.next();
                    self.shapes
                        .push_str(&table(id, r, t, &self.font(18.0), &self.theme.accent));
                }
            }
            "chart" => {
                if let Some(c) = &e.chart {
                    self.chart(r, c);
                }
            }
            "shape" => {
                let f = Font {
                    color: "#ffffff".into(),
                    ..self.font(e.size.unwrap_or(20.0))
                };
                let id = self.next();
                let fill = e.fill.clone().unwrap_or_else(|| self.theme.accent.clone());
                self.shapes
                    .push_str(&shape(id, r, &fill, true, &para(&e.text, &f, "ctr")));
            }
            _ => {
                let f = Font {
                    color: e
                        .color
                        .clone()
                        .unwrap_or_else(|| self.theme.foreground.clone()),
                    ..self.font(e.size.unwrap_or(24.0))
                };
                self.text("Text", r, &para(&e.text, &f, "l"), "t");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn body(&mut self, s: &Slide) {
        let muted = self.theme.muted.clone();
        match s.layout {
            Layout::Title => {
                let id = self.next();
                self.shapes.push_str(&shape(
                    id,
                    (96.0, 226.0, 120.0, 7.0),
                    &self.theme.accent,
                    true,
                    "",
                ));
                let f = Font {
                    bold: true,
                    ..self.font(60.0)
                };
                self.text(
                    "Center Title",
                    (96.0, 250.0, 1088.0, 170.0),
                    &para(&s.title, &f, "l"),
                    "t",
                );
                let f = Font {
                    color: muted,
                    ..self.font(28.0)
                };
                self.text(
                    "Subtitle",
                    (96.0, 430.0, 1088.0, 120.0),
                    &para(&s.subtitle, &f, "l"),
                    "t",
                );
            }
            Layout::Section => {
                let f = Font {
                    bold: true,
                    color: "#ffffff".into(),
                    ..self.font(54.0)
                };
                self.text(
                    "Title",
                    (96.0, 250.0, 1088.0, 160.0),
                    &para(&s.title, &f, "l"),
                    "b",
                );
                let f = Font {
                    color: "#e5e7eb".into(),
                    ..self.font(26.0)
                };
                self.text(
                    "Subtitle",
                    (96.0, 430.0, 1088.0, 100.0),
                    &para(&s.subtitle, &f, "l"),
                    "t",
                );
            }
            Layout::Bullets => {
                self.title(s);
                self.bullets(BODY, &s.bullets, 26.0);
            }
            Layout::TwoColumn => {
                self.title(s);
                self.bullets(LEFT, &s.left, 24.0);
                self.bullets(RIGHT, &s.right, 24.0);
            }
            Layout::Image => {
                self.title(s);
                let r = if s.caption.is_empty() {
                    BODY
                } else {
                    (64.0, 172.0, 1152.0, 444.0)
                };
                self.image(r, s.image.as_deref().unwrap_or_default());
                if !s.caption.is_empty() {
                    let f = Font {
                        color: muted,
                        ..self.font(18.0)
                    };
                    self.text(
                        "Caption",
                        (64.0, 628.0, 1152.0, 36.0),
                        &para(&s.caption, &f, "ctr"),
                        "t",
                    );
                }
            }
            Layout::ImageText => {
                self.title(s);
                self.image(LEFT, s.image.as_deref().unwrap_or_default());
                self.bullets(RIGHT, &s.bullets, 24.0);
            }
            Layout::Table => {
                self.title(s);
                if let Some(t) = &s.table {
                    let id = self.next();
                    self.shapes
                        .push_str(&table(id, BODY, t, &self.font(18.0), &self.theme.accent));
                }
            }
            Layout::Chart => {
                self.title(s);
                if let Some(c) = &s.chart {
                    self.chart(BODY, c);
                }
            }
            Layout::Quote => {
                let f = Font {
                    italic: true,
                    ..self.font(40.0)
                };
                self.text(
                    "Quote",
                    (128.0, 150.0, 1024.0, 360.0),
                    &para(&format!("“{}”", s.quote), &f, "l"),
                    "ctr",
                );
                if !s.by.is_empty() {
                    let f = Font {
                        color: muted,
                        ..self.font(24.0)
                    };
                    self.text(
                        "By",
                        (128.0, 530.0, 1024.0, 60.0),
                        &para(&format!("— {}", s.by), &f, "l"),
                        "t",
                    );
                }
            }
            Layout::BigNumber => {
                self.title(s);
                let f = Font {
                    bold: true,
                    color: self.theme.accent.clone(),
                    ..self.font(150.0)
                };
                self.text(
                    "Number",
                    (64.0, 190.0, 1152.0, 260.0),
                    &para(&s.number, &f, "ctr"),
                    "b",
                );
                let f = Font {
                    color: muted,
                    ..self.font(30.0)
                };
                self.text(
                    "Label",
                    (64.0, 470.0, 1152.0, 120.0),
                    &para(&s.label, &f, "ctr"),
                    "t",
                );
            }
            Layout::Free => {
                self.title(s);
                for e in &s.elements {
                    self.element(e);
                }
            }
        }
    }

    pub(super) fn chrome(&mut self, s: &Slide, i: usize, total: usize) {
        if matches!(s.layout, Layout::Title | Layout::Section) {
            return;
        }
        let f = Font {
            color: self.theme.muted.clone(),
            ..self.font(13.0)
        };
        if let Some(footer) = self.theme.footer.clone().filter(|t| !t.is_empty()) {
            self.text(
                "Footer",
                (64.0, 680.0, 900.0, 24.0),
                &para(&footer, &f, "l"),
                "t",
            );
        }
        self.text(
            "Slide number",
            (1100.0, 680.0, 116.0, 24.0),
            &para(&format!("{} / {total}", i + 1), &f, "r"),
            "t",
        );
        if let Some(logo) = self.theme.logo.clone() {
            self.image((1136.0, 30.0, 80.0, 40.0), &logo);
        }
    }
}
