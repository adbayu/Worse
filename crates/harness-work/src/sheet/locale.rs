//! Workbook locales: which `IronCalc` locale the engine runs with, and how numbers are shown for
//! languages the engine does not ship (`IronCalc` 0.9 has en, de, es, fr and it).

struct Locale {
    lang: &'static str,
    /// Windows LCIDs used in number formats such as `[$Rp-421]`.
    lcids: &'static [u32],
    /// Built-in engine locale.
    engine: &'static str,
    decimal: char,
    group: char,
}

const fn loc(
    lang: &'static str,
    lcids: &'static [u32],
    engine: &'static str,
    decimal: char,
    group: char,
) -> Locale {
    Locale {
        lang,
        lcids,
        engine,
        decimal,
        group,
    }
}

const NBSP: char = '\u{a0}';

const LOCALES: &[Locale] = &[
    loc(
        "en",
        &[0x409, 0x809, 0xC09, 0x1009, 0x1409, 0x4809],
        "en",
        '.',
        ',',
    ),
    loc("de", &[0x407, 0xC07], "de", ',', '.'),
    loc("es", &[0x40A, 0xC0A], "es", ',', '.'),
    loc("fr", &[0x40C, 0x80C], "fr", ',', NBSP),
    loc("it", &[0x410], "it", ',', '.'),
    loc("id", &[0x421], "en", ',', '.'),
    loc("nl", &[0x413, 0x813], "en", ',', '.'),
    loc("pt", &[0x416, 0x816], "en", ',', '.'),
    loc("pl", &[0x415], "en", ',', NBSP),
    loc("tr", &[0x41F], "en", ',', '.'),
    loc("vi", &[0x42A], "en", ',', '.'),
    loc("da", &[0x406], "en", ',', '.'),
    loc("sv", &[0x41D], "en", ',', NBSP),
    loc("nb", &[0x414], "en", ',', NBSP),
    loc("ru", &[0x419], "en", ',', NBSP),
];

fn find(tag: &str) -> Option<&'static Locale> {
    let lang = tag.split(['-', '_']).next()?.to_ascii_lowercase();
    LOCALES.iter().find(|l| l.lang == lang)
}

/// Engine locale for a workbook locale tag (`id-ID` → `en`, `de-AT` → `de`).
pub(crate) fn engine(tag: &str) -> &'static str {
    find(tag).map_or("en", |l| l.engine)
}

/// Language of a Windows LCID from a number format (`0x421` → `id`).
pub(crate) fn from_lcid(lcid: u32) -> Option<&'static str> {
    let id = lcid & 0xFFFF;
    LOCALES
        .iter()
        .find(|l| l.lcids.contains(&id))
        .map(|l| l.lang)
}

/// Rewrites the separators of a number the engine formatted in English (`1,234.5`) for the
/// workbook language (`1.234,5` for `id`). Only separators between two digits change, so text
/// in the format (`Rp`, `kg`, `Nov 22, 2026`) stays as it is.
pub(crate) fn localize(tag: &str, engine_locale: &str, shown: String) -> String {
    let Some(l) = find(tag) else {
        return shown;
    };
    if l.decimal == '.' || !engine_locale.eq_ignore_ascii_case("en") {
        return shown;
    }
    let chars: Vec<char> = shown.chars().collect();
    let digit = |i: Option<usize>| {
        i.and_then(|i| chars.get(i))
            .is_some_and(char::is_ascii_digit)
    };
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let between = digit(i.checked_sub(1)) && digit(Some(i + 1));
            match c {
                '.' if between => l.decimal,
                ',' if between => l.group,
                other => other,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localizes_numbers() {
        assert_eq!(localize("id-ID", "en", "Rp 1,234.50".into()), "Rp 1.234,50");
        assert_eq!(localize("id", "en", "12.5 kg".into()), "12,5 kg");
        assert_eq!(localize("id", "en", "Nov 22, 2026".into()), "Nov 22, 2026");
        assert_eq!(localize("en-US", "en", "1,234.5".into()), "1,234.5");
        assert_eq!(localize("de", "de", "1.234,5".into()), "1.234,5");
        assert_eq!(engine("de-AT"), "de");
        assert_eq!(engine("id-ID"), "en");
        assert_eq!(from_lcid(0x421), Some("id"));
    }
}
