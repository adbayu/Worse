//! Pure helpers shared by the settings pages: dates, folder labels, colours, paths, list parsing.

use super::{BTreeMap, Path};

/// `YYYY-MM-DD` (UTC) of a unix-ms timestamp; language-neutral, so no translation is needed.
pub(crate) fn date_of(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    // Civil-from-days (H. Hinnant), valid for the whole i64 day range used here.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub(super) fn folder_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

pub(crate) fn opt(value: Option<&String>) -> String {
    value.cloned().unwrap_or_default()
}

pub(super) fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(super) fn lines_of(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub(super) fn pairs(value: &str, sep: char) -> BTreeMap<String, String> {
    lines_of(value)
        .into_iter()
        .filter_map(|l| {
            l.split_once(sep)
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Last two segments of a path ("D:\\work\\`ZCode`\\Worse" -> "ZCode/Worse").
pub(super) fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    !b.is_empty() && norm(a) == norm(b)
}

/// "#RRGGBB" → colour (settings S2 accent swatches).
/// "#RRGGBB" → colour (settings S2 accent swatches).
pub(super) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
    let n = parts.len();
    parts[n.saturating_sub(2)..].join("/")
}

#[allow(clippy::many_single_char_names)]
pub(super) fn parse_hex(s: &str) -> Option<slint::Color> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    let [_, r, g, b] = n.to_be_bytes();
    Some(slint::Color::from_rgb_u8(r, g, b))
}

/// API keys pasted into the Command Code dialog: one per line (commas and semicolons also separate), blanks
/// and repeats dropped, order kept.
pub(super) fn command_code_keys(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in text
        .split(['\n', '\r', ',', ';'])
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        if !out.iter().any(|k| k == key) {
            out.push(key.to_owned());
        }
    }
    out
}
