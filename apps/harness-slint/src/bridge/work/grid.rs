//! Excel look of the sheet grid (worker thread): column widths, merged cells, text spilling into
//! empty neighbours, fills, font colors, alignment, borders and font sizes from the workbook.

use harness_work::{Sheet, SheetGrid};

use super::sv;
use crate::ui::SheetCellView;

/// Row height in px; the Slint grid uses the same value.
pub(super) const ROW_PX: f32 = 22.0;

#[allow(clippy::struct_excessive_bools)]
pub(super) struct LoadedCell {
    pub coord: String,
    pub display: String,
    pub formula: String,
    pub bold: bool,
    pub number: bool,
    pub error: bool,
    pub fill: Option<[u8; 3]>,
    pub fg: Option<[u8; 3]>,
    /// 0 auto, 1 left, 2 center, 3 right.
    pub align: i32,
    /// Border widths in px: top, right, bottom, left.
    pub borders: [f32; 4],
    pub scale: f32,
    pub width: f32,
    pub off_x: f32,
    pub off_y: f32,
    pub span_w: f32,
    pub span_h: f32,
    /// Shows text spilled over from a cell to the left; clicking it selects an empty cell.
    pub ghost: bool,
    pub italic: bool,
    pub wrap: bool,
    pub bc: Option<[u8; 3]>,
    pub note: String,
}

pub(super) struct LoadedRow {
    pub row_num: i32,
    pub height: f32,
    pub cells: Vec<LoadedCell>,
}

#[allow(clippy::cast_possible_truncation)]
fn col_px(chars: f64) -> f32 {
    if chars <= 0.0 {
        0.0
    } else {
        (chars * 7.0 + 5.0).clamp(24.0, 480.0) as f32
    }
}

#[allow(clippy::cast_possible_truncation)]
fn row_px(pt: f64) -> f32 {
    if pt <= 0.0 {
        0.0
    } else {
        (pt * 1.35 + 2.0).clamp(16.0, 140.0) as f32
    }
}

fn hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let [_, red, green, blue] = u32::from_str_radix(h, 16).ok()?.to_be_bytes();
    Some([red, green, blue])
}

/// Text color: the workbook color on a fill (black by default); on the app background only
/// colors that stay readable there.
fn text_color(color: Option<&str>, fill: Option<[u8; 3]>) -> Option<[u8; 3]> {
    let fg = color.and_then(hex);
    if fill.is_some() {
        return Some(fg.unwrap_or([0, 0, 0]));
    }
    fg.filter(|[r, g, b]| {
        let lum = 0.299 * f32::from(*r) + 0.587 * f32::from(*g) + 0.114 * f32::from(*b);
        lum > 70.0
    })
}

/// Zero-based (row, column) of A1.
fn cell_rc(name: &str) -> Option<(usize, usize)> {
    let (r, c) = harness_work::parse_cell(name)?;
    Some((usize::try_from(r - 1).ok()?, usize::try_from(c - 1).ok()?))
}

/// Builds the visible grid rows and column widths in px (hidden rows/columns omitted).
pub(super) fn build(grid: &SheetGrid, sheet: Option<&Sheet>) -> (Vec<(i32, f32)>, Vec<LoadedRow>) {
    let widths: Vec<f32> = grid.widths.iter().map(|w| col_px(*w)).collect();
    let heights: Vec<f32> = grid
        .heights
        .iter()
        .map(|h| row_px(*h))
        .chain(std::iter::repeat(ROW_PX))
        .take(grid.rows.len())
        .collect();
    let mut rows: Vec<LoadedRow> = grid
        .rows
        .iter()
        .enumerate()
        .map(|(r, row)| {
            let height = heights.get(r).copied().unwrap_or(ROW_PX);
            LoadedRow {
                row_num: grid.first_row + i32::try_from(r).unwrap_or(0),
                height,
                cells: row
                    .iter()
                    .enumerate()
                    .map(|(c, cell)| {
                        let fill = cell.fill.as_deref().and_then(hex);
                        let width = widths.get(c).copied().unwrap_or(64.0);
                        let side = |ch: char| {
                            if cell.border.contains(ch.to_ascii_uppercase()) {
                                2.0
                            } else if cell.border.contains(ch) {
                                1.0
                            } else {
                                0.0
                            }
                        };
                        #[allow(clippy::cast_possible_truncation)]
                        let scale = if cell.size > 0.0 {
                            (cell.size / 11.0).clamp(0.6, 2.4) as f32
                        } else {
                            1.0
                        };
                        LoadedCell {
                            coord: format!(
                                "{}{}",
                                super::load::col_name(
                                    grid.first_col + i32::try_from(c).unwrap_or(0)
                                ),
                                grid.first_row + i32::try_from(r).unwrap_or(0)
                            ),
                            display: cell.text.clone(),
                            formula: cell.formula.clone(),
                            bold: cell.bold,
                            number: cell.number,
                            error: cell.error,
                            fg: text_color(cell.color.as_deref(), fill),
                            fill,
                            align: match cell.align.as_str() {
                                "left" => 1,
                                "center" => 2,
                                "right" => 3,
                                _ => 0,
                            },
                            borders: [side('t'), side('r'), side('b'), side('l')],
                            scale,
                            width,
                            off_x: 0.0,
                            off_y: 0.0,
                            span_w: width,
                            span_h: height,
                            ghost: false,
                            italic: cell.italic,
                            wrap: cell.wrap,
                            bc: cell.border_color.as_deref().and_then(hex),
                            note: cell.note.clone(),
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    let mut merged = vec![vec![false; widths.len()]; rows.len()];
    for range in sheet.map(|s| s.merges.as_slice()).unwrap_or_default() {
        merge(&mut rows, &mut merged, &widths, &heights, range);
    }
    spill(&mut rows, &merged);
    // Drop hidden columns and hidden rows so the Slint grid matches Excel's visible layout.
    let vis_cols: Vec<(i32, f32)> = widths
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0.0)
        .map(|(c, w)| (grid.first_col + i32::try_from(c).unwrap_or(0), *w))
        .collect();
    for row in &mut rows {
        row.cells.retain(|cell| cell.width > 0.0);
    }
    rows.retain(|row| row.height > 0.0);
    (vis_cols, rows)
}

/// Every cell of a merge shows its slice of the anchor (top-left) cell.
fn merge(
    rows: &mut [LoadedRow],
    merged: &mut [Vec<bool>],
    widths: &[f32],
    heights: &[f32],
    range: &str,
) {
    let Some((a, b)) = range.split_once(':') else {
        return;
    };
    let (Some((r1, c1)), Some((r2, c2))) = (cell_rc(a), cell_rc(b)) else {
        return;
    };
    let Some(anchor) = rows.get(r1).and_then(|row| row.cells.get(c1)) else {
        return;
    };
    let r2 = r2.min(rows.len().saturating_sub(1));
    let c2 = c2.min(widths.len().saturating_sub(1));
    let span_w: f32 = widths.get(c1..=c2).map_or(0.0, |w| w.iter().sum());
    let span_h: f32 = heights.get(r1..=r2).map_or(ROW_PX, |h| h.iter().sum());
    let (coord, display, formula) = (
        anchor.coord.clone(),
        anchor.display.clone(),
        anchor.formula.clone(),
    );
    let (bold, italic, wrap, number, error, fill, fg, align, scale) = (
        anchor.bold,
        anchor.italic,
        anchor.wrap,
        anchor.number,
        anchor.error,
        anchor.fill,
        anchor.fg,
        anchor.align,
        anchor.scale,
    );
    for r in r1..=r2 {
        for c in c1..=c2 {
            let Some(cell) = rows.get_mut(r).and_then(|row| row.cells.get_mut(c)) else {
                continue;
            };
            cell.coord.clone_from(&coord);
            cell.display.clone_from(&display);
            cell.formula.clone_from(&formula);
            (
                cell.bold,
                cell.italic,
                cell.wrap,
                cell.number,
                cell.error,
                cell.align,
                cell.scale,
            ) = (bold, italic, wrap, number, error, align, scale);
            cell.fill = cell.fill.or(fill);
            cell.fg = if cell.fill.is_some() {
                fg.or(Some([0, 0, 0]))
            } else {
                fg
            };
            cell.off_x = widths.get(c1..c).map_or(0.0, |w| w.iter().sum());
            cell.off_y = heights.get(r1..r).map_or(0.0, |h| h.iter().sum());
            cell.span_w = span_w;
            cell.span_h = span_h;
            if let Some(m) = merged.get_mut(r).and_then(|row| row.get_mut(c)) {
                *m = true;
            }
        }
    }
}

/// Left-aligned text runs on over empty cells to its right, like in Excel.
fn spill(rows: &mut [LoadedRow], merged: &[Vec<bool>]) {
    for (r, row) in rows.iter_mut().enumerate() {
        let is_merged = |c: usize| {
            merged
                .get(r)
                .and_then(|m| m.get(c))
                .copied()
                .unwrap_or(false)
        };
        let mut c = 0;
        while c < row.cells.len() {
            let src = &row.cells[c];
            let spills = !is_merged(c)
                && !src.display.is_empty()
                && !src.number
                && matches!(src.align, 0 | 1);
            if !spills {
                c += 1;
                continue;
            }
            let (text, bold, fg, scale) = (src.display.clone(), src.bold, src.fg, src.scale);
            let mut end = c + 1;
            while end < row.cells.len() && row.cells[end].display.is_empty() && !is_merged(end) {
                end += 1;
            }
            let span: f32 = row.cells[c..end].iter().map(|x| x.width).sum();
            let mut off = 0.0;
            for (i, cell) in row.cells[c..end].iter_mut().enumerate() {
                cell.span_w = span;
                cell.off_x = off;
                off += cell.width;
                if i > 0 {
                    cell.display.clone_from(&text);
                    (cell.bold, cell.scale, cell.ghost) = (bold, scale, true);
                    cell.fg = if cell.fill.is_some() {
                        fg.or(Some([0, 0, 0]))
                    } else {
                        fg
                    };
                    cell.align = 1;
                }
            }
            c = end;
        }
    }
}

fn color(rgb: Option<[u8; 3]>) -> (slint::Color, bool) {
    rgb.map_or((slint::Color::default(), false), |[r, g, b]| {
        (slint::Color::from_rgb_u8(r, g, b), true)
    })
}

/// Slint row model item (UI thread).
pub(super) fn to_view(c: &LoadedCell) -> SheetCellView {
    let (fill, has_fill) = color(c.fill);
    let (fg, has_fg) = color(c.fg);
    let (bc, has_bc) = color(c.bc);
    let [bt, br, bb, bl] = c.borders;
    SheetCellView {
        coord: sv(&c.coord),
        display: sv(&c.display),
        formula: sv(&c.formula),
        bold: c.bold,
        number: c.number,
        error: c.error,
        fill,
        has_fill,
        fg,
        has_fg,
        align: c.align,
        bt,
        br,
        bb,
        bl,
        scale: c.scale,
        width: c.width,
        off_x: c.off_x,
        off_y: c.off_y,
        span_w: c.span_w,
        span_h: c.span_h,
        ghost: c.ghost,
        italic: c.italic,
        wrap: c.wrap,
        bc,
        has_bc,
        has_note: !c.note.is_empty(),
    }
}

/// Returns validation list options for `coord` if it falls inside any validation range.
pub(super) fn validation_for(coord: &str, rules: &[(String, Vec<String>)]) -> Vec<String> {
    let Some((r, c)) = cell_rc(coord) else {
        return Vec::new();
    };
    for (range, opts) in rules {
        let (a, b) = range.split_once(':').unwrap_or((range, range));
        let (Some((r1, c1)), Some((r2, c2))) = (cell_rc(a), cell_rc(b)) else {
            continue;
        };
        if (r1.min(r2)..=r1.max(r2)).contains(&r) && (c1.min(c2)..=c1.max(c2)).contains(&c) {
            return opts.clone();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_cells_and_colors() {
        assert_eq!(cell_rc("B1"), Some((0, 1)));
        assert_eq!(cell_rc("AA10"), Some((9, 26)));
        assert_eq!(hex("#FFD966"), Some([0xFF, 0xD9, 0x66]));
        assert_eq!(text_color(None, Some([1, 2, 3])), Some([0, 0, 0]));
        assert_eq!(text_color(Some("#000000"), None), None);
        assert_eq!(text_color(Some("#FF0000"), None), Some([255, 0, 0]));
    }
}
