//! Cell look beyond values and fills: font colors, horizontal alignment, borders and font sizes,
//! both ways between `book.json` and the `IronCalc` style (import, evaluation, .xlsx export).

use ironcalc::base::types::{BorderItem, BorderStyle, Color, HorizontalAlignment, Style, Theme};
use ironcalc::base::Model;

use crate::error::WorkResult;
use crate::model::book::Sheet;

/// `IronCalc` default font size; only other sizes are stored.
pub(crate) const DEFAULT_SIZE: i32 = 12;

/// `left`, `center`, `right`, or empty for Excel's "General".
pub(crate) fn align_name(st: &Style) -> &'static str {
    match st.alignment.as_ref().map(|a| &a.horizontal) {
        Some(HorizontalAlignment::Center | HorizontalAlignment::CenterContinuous) => "center",
        Some(HorizontalAlignment::Left) => "left",
        Some(HorizontalAlignment::Right) => "right",
        _ => "",
    }
}

pub(crate) fn set_align(st: &mut Style, name: &str) {
    let horizontal = match name {
        "center" => HorizontalAlignment::Center,
        "left" => HorizontalAlignment::Left,
        "right" => HorizontalAlignment::Right,
        _ => return,
    };
    let mut a = st.alignment.clone().unwrap_or_default();
    a.horizontal = horizontal;
    st.alignment = Some(a);
}

/// Sides `t r b l` that have a line; upper case for a thick (medium, thick, double) one.
pub(crate) fn border_code(st: &Style) -> String {
    let b = &st.border;
    [
        (&b.top, 't'),
        (&b.right, 'r'),
        (&b.bottom, 'b'),
        (&b.left, 'l'),
    ]
    .into_iter()
    .filter_map(|(side, ch)| {
        side.as_ref().map(|item| match item.style {
            BorderStyle::Medium | BorderStyle::Thick | BorderStyle::Double => {
                ch.to_ascii_uppercase()
            }
            _ => ch,
        })
    })
    .collect()
}

pub(crate) fn set_borders(st: &mut Style, code: &str) {
    for ch in code.chars() {
        let item = Some(BorderItem {
            style: if ch.is_ascii_uppercase() {
                BorderStyle::Medium
            } else {
                BorderStyle::Thin
            },
            color: Color::Rgb("#000000".into()),
        });
        match ch.to_ascii_lowercase() {
            't' => st.border.top = item,
            'r' => st.border.right = item,
            'b' => st.border.bottom = item,
            'l' => st.border.left = item,
            _ => {}
        }
    }
}

/// `#RRGGBB` of a style color, theme colors resolved; `None` when unset.
pub(crate) fn rgb(color: &Color, theme: &Theme) -> Option<String> {
    let hex = color.to_rgb(theme);
    (hex.len() == 7 && hex.starts_with('#')).then(|| hex.to_ascii_uppercase())
}

/// Writes the `book.json` look of one sheet into the model (fills and bold are done by the caller).
pub(super) fn apply(model: &mut Model<'static>, idx: u32, s: &Sheet) -> WorkResult<()> {
    for (range, hex) in &s.colors {
        if let Some(color) = super::hex_color(hex) {
            super::style_range(model, idx, range, |st| st.font.color = color.clone())?;
        }
    }
    for (range, name) in &s.align {
        super::style_range(model, idx, range, |st| set_align(st, name))?;
    }
    for (range, code) in &s.borders {
        super::style_range(model, idx, range, |st| set_borders(st, code))?;
    }
    for (range, size) in &s.sizes {
        #[allow(clippy::cast_possible_truncation)]
        let sz = size.round().clamp(1.0, 409.0) as i32;
        super::style_range(model, idx, range, |st| st.font.sz = sz)?;
    }
    Ok(())
}

/// Records the look of an imported cell into `sheet` (`base_size` = the workbook's usual size).
pub(crate) fn record(sheet: &mut Sheet, key: &str, st: &Style, theme: &Theme, base_size: i32) {
    if let Some(fill) = rgb(&st.fill.color, theme) {
        sheet.fills.insert(key.to_owned(), fill);
    }
    if let Some(color) = rgb(&st.font.color, theme).filter(|c| c != "#000000") {
        sheet.colors.insert(key.to_owned(), color);
    }
    let align = align_name(st);
    if !align.is_empty() {
        sheet.align.insert(key.to_owned(), align.to_owned());
    }
    let border = border_code(st);
    if !border.is_empty() {
        sheet.borders.insert(key.to_owned(), border);
    }
    if st.font.sz != base_size && st.font.sz > 0 {
        sheet.sizes.insert(key.to_owned(), f64::from(st.font.sz));
    }
    if st.font.i {
        sheet.italic.push(key.to_owned());
    }
    if st.font.u {
        sheet.underline.push(key.to_owned());
    }
    if st.alignment.as_ref().is_some_and(|a| a.wrap_text) {
        sheet.wrap.push(key.to_owned());
    }
    let b = &st.border;
    if let Some(color) = [&b.top, &b.right, &b.bottom, &b.left]
        .into_iter()
        .flatten()
        .find_map(|item| rgb(&item.color, theme).filter(|c| c != "#000000"))
    {
        sheet.border_colors.insert(key.to_owned(), color);
    }
}

/// Font color, alignment, border code and size (0 = default) of a cell for the grid UI.
pub(super) fn look(st: &Style) -> (Option<String>, String, String, f64) {
    let color = match &st.font.color {
        Color::Rgb(h) => Some(h.clone()),
        _ => None,
    };
    let size = if st.font.sz == DEFAULT_SIZE {
        0.0
    } else {
        f64::from(st.font.sz)
    };
    (color, align_name(st).to_owned(), border_code(st), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borders_and_align_round_trip() {
        let mut st = Style::default();
        set_borders(&mut st, "TrbL");
        assert_eq!(border_code(&st), "TrbL");
        set_align(&mut st, "center");
        assert_eq!(align_name(&st), "center");
        assert_eq!(align_name(&Style::default()), "");
    }
}
