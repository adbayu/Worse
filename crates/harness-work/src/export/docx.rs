//! `doc.md` → `.docx` through docx-rs: real heading styles (so Word's navigation and TOC work),
//! tables, images, charts as pictures, footnotes, callouts, header/footer with page numbers.

use docx_rs::{
    AlignmentType, BreakType, Docx, Footer, Footnote, Header, Hyperlink, HyperlinkType, PageMargin,
    PageNum, Paragraph, Pic, Run, RunFonts, Shading, ShdType, Style, StyleType, Table, TableCell,
    TableOfContents, TableRow,
};

use super::ooxml::hex;
use super::pptx_shapes::image_size;
use crate::error::{WorkError, WorkResult};
use crate::item::WorkItem;
use crate::model::doc::{Block, Doc, Inline};

const EMU_PER_PX: f64 = 9525.0;
/// Text width of an A4 page with the default margin, in pixels at 96 dpi.
const TEXT_WIDTH_PX: f64 = 600.0;

fn run_of(r: &crate::model::text::Run) -> Run {
    let mut run = Run::new().add_text(&r.text);
    if r.bold {
        run = run.bold();
    }
    if r.italic {
        run = run.italic();
    }
    if r.code {
        run = run
            .fonts(RunFonts::new().ascii("Consolas").hi_ansi("Consolas"))
            .shading(Shading::new().shd_type(ShdType::Clear).fill("F3F4F6"));
    }
    run
}

fn inlines(mut p: Paragraph, items: &[Inline], bold: bool) -> Paragraph {
    for i in items {
        p = match i {
            Inline::Text(r) => {
                let run = run_of(r);
                p.add_run(if bold { run.bold() } else { run })
            }
            Inline::Link { text, url } => p.add_hyperlink(
                Hyperlink::new(url, HyperlinkType::External).add_run(
                    Run::new()
                        .add_text(text)
                        .color("2563EB")
                        .underline("single"),
                ),
            ),
            Inline::Note(note) => {
                let mut f = Footnote::new();
                let f = f.add_content(inlines(Paragraph::new(), note, false));
                p.add_run(Run::new().add_footnote_reference(f))
            }
            Inline::Break => p.add_run(Run::new().add_break(BreakType::TextWrapping)),
        };
    }
    p
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_pass_by_value
)]
fn picture(bytes: Vec<u8>) -> Run {
    let (w, h) = image_size(&bytes).unwrap_or((TEXT_WIDTH_PX, TEXT_WIDTH_PX * 0.5));
    let k = (TEXT_WIDTH_PX / w).min(1.0);
    let pic = Pic::new(&bytes).size((w * k * EMU_PER_PX) as u32, (h * k * EMU_PER_PX) as u32);
    Run::new().add_image(pic)
}

fn heading_styles(accent: &str) -> Vec<Style> {
    [(1, 36), (2, 30), (3, 26), (4, 24), (5, 22), (6, 22)]
        .into_iter()
        .map(|(lvl, size)| {
            let s = Style::new(format!("Heading{lvl}"), StyleType::Paragraph)
                .name(format!("Heading {lvl}"))
                .size(size)
                .bold()
                .outline_lvl(lvl - 1);
            if lvl == 1 {
                s.color(accent)
            } else {
                s
            }
        })
        .collect()
}

fn cell(items: &[Inline], header: bool) -> TableCell {
    let c = TableCell::new().add_paragraph(inlines(Paragraph::new(), items, header));
    if header {
        c.shading(Shading::new().shd_type(ShdType::Clear).fill("E5E7EB"))
    } else {
        c
    }
}

/// Writes the doc as a `.docx` file body.
#[allow(clippy::too_many_lines)]
pub(crate) fn docx(item: &WorkItem, doc: &Doc, accent: &str) -> WorkResult<Vec<u8>> {
    let m = &doc.meta;
    let accent = hex(accent, "3B6CF6");
    let mut d = Docx::new()
        .default_fonts(RunFonts::new().ascii(&m.font).hi_ansi(&m.font))
        .default_size(22);
    for s in heading_styles(&accent) {
        d = d.add_style(s);
    }
    // 页边距：厘米 → twip（1cm = 567 twip）。
    #[allow(clippy::cast_possible_truncation)]
    let margin = (m.margin.clamp(0.5, 6.0) * 567.0) as i32;
    d = d.page_margin(
        PageMargin::new()
            .top(margin)
            .bottom(margin)
            .left(margin)
            .right(margin),
    );
    if m.paper == "letter" {
        d = d.page_size(12240, 15840);
    }
    if !m.header.is_empty() {
        d = d.header(
            Header::new().add_paragraph(
                Paragraph::new()
                    .align(AlignmentType::Right)
                    .add_run(Run::new().add_text(&m.header).size(18).color("6E6E6E")),
            ),
        );
    }
    let mut foot = Paragraph::new().align(AlignmentType::Center);
    if !m.footer.is_empty() {
        foot = foot.add_run(
            Run::new()
                .add_text(format!("{}    ", m.footer))
                .size(18)
                .color("6E6E6E"),
        );
    }
    d = d.footer(Footer::new().add_paragraph(foot.add_page_num(PageNum::new())));
    if m.cover {
        d = d
            .add_paragraph(
                Paragraph::new().add_run(
                    Run::new()
                        .add_break(BreakType::TextWrapping)
                        .add_break(BreakType::TextWrapping)
                        .add_break(BreakType::TextWrapping),
                ),
            )
            .add_paragraph(
                Paragraph::new().add_run(Run::new().add_text(doc.title()).size(64).bold()),
            )
            .add_paragraph(
                Paragraph::new().add_run(Run::new().add_text(&m.header).size(28).color("6E6E6E")),
            )
            .add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
    }
    if m.toc {
        d = d.add_table_of_contents(TableOfContents::new().heading_styles_range(1, 3).auto());
        d = d.add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)));
    }
    for b in doc.blocks() {
        d = match b {
            Block::Heading { level, text, .. } => d.add_paragraph(inlines(
                Paragraph::new().style(&format!("Heading{}", level.clamp(1, 6))),
                &text,
                false,
            )),
            Block::Para(text) => d.add_paragraph(inlines(Paragraph::new(), &text, false)),
            Block::List { ordered, items } => {
                let mut n = [0usize; 8];
                for (depth, text) in items {
                    let lvl = usize::from(depth).min(7);
                    n[lvl] += 1;
                    n.iter_mut().skip(lvl + 1).for_each(|x| *x = 0);
                    let marker = if ordered {
                        format!("{}.\t", n[lvl])
                    } else {
                        "•\t".to_string()
                    };
                    let indent = 360 + i32::from(depth) * 360;
                    let p = Paragraph::new()
                        .indent(
                            Some(indent),
                            Some(docx_rs::SpecialIndentType::Hanging(360)),
                            None,
                            None,
                        )
                        .add_run(Run::new().add_text(marker));
                    d = d.add_paragraph(inlines(p, &text, false));
                }
                d
            }
            Block::Table { header, rows } => {
                let cols = rows
                    .iter()
                    .map(Vec::len)
                    .chain(std::iter::once(header.len()))
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let mut trs = Vec::new();
                if !header.is_empty() {
                    trs.push(TableRow::new(
                        (0..cols)
                            .map(|c| cell(header.get(c).map_or(&[][..], Vec::as_slice), true))
                            .collect(),
                    ));
                }
                for row in &rows {
                    trs.push(TableRow::new(
                        (0..cols)
                            .map(|c| cell(row.get(c).map_or(&[][..], Vec::as_slice), false))
                            .collect(),
                    ));
                }
                d.add_table(Table::new(trs)).add_paragraph(Paragraph::new())
            }
            Block::Image { src, caption, .. } => {
                let p = match item.asset(&src).and_then(|p| std::fs::read(p).ok()) {
                    Some(bytes) => Paragraph::new()
                        .align(AlignmentType::Center)
                        .add_run(picture(bytes)),
                    None => Paragraph::new().add_run(
                        Run::new()
                            .add_text(format!("[Missing image: {src}]"))
                            .italic(),
                    ),
                };
                let d = d.add_paragraph(p);
                if caption.is_empty() {
                    d
                } else {
                    d.add_paragraph(
                        Paragraph::new()
                            .align(AlignmentType::Center)
                            .add_run(Run::new().add_text(caption).italic().size(18)),
                    )
                }
            }
            Block::Code { text, .. } => {
                let mut d = d;
                for line in text.lines() {
                    d = d.add_paragraph(
                        Paragraph::new().add_run(
                            Run::new()
                                .add_text(line)
                                .fonts(RunFonts::new().ascii("Consolas").hi_ansi("Consolas"))
                                .size(19)
                                .shading(Shading::new().shd_type(ShdType::Clear).fill("F3F4F6")),
                        ),
                    );
                }
                d
            }
            Block::Chart(chart) => {
                let resolved = crate::render::data::resolve_chart(item, &chart);
                let svg = crate::render::chart_svg::svg(
                    &resolved,
                    600.0,
                    320.0,
                    &crate::render::chart_svg::Style::default(),
                );
                let png = crate::render::svg_png(svg.into_bytes(), 600.0, 320.0, 2.0)?;
                d.add_paragraph(
                    Paragraph::new()
                        .align(AlignmentType::Center)
                        .add_run(picture(png)),
                )
            }
            Block::Callout { kind, text } => {
                let title = if kind.is_empty() {
                    String::new()
                } else {
                    format!("{}: ", kind.to_uppercase())
                };
                let p = Paragraph::new()
                    .add_run(Run::new().add_text(title).bold().color(accent.clone()));
                let p = inlines(p, &text, false);
                d.add_table(Table::new(vec![TableRow::new(vec![TableCell::new()
                    .add_paragraph(p)
                    .shading(Shading::new().shd_type(ShdType::Clear).fill("EEF2FF"))])]))
                    .add_paragraph(Paragraph::new())
            }
            Block::PageBreak => {
                d.add_paragraph(Paragraph::new().add_run(Run::new().add_break(BreakType::Page)))
            }
            Block::Rule => d.add_paragraph(
                Paragraph::new()
                    .align(AlignmentType::Center)
                    .add_run(Run::new().add_text("⁂").color("9CA3AF")),
            ),
        };
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    d.build()
        .pack(&mut buf)
        .map_err(|e| WorkError::Export(e.to_string()))?;
    Ok(buf.into_inner())
}
