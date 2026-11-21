//! Content-based file type detection for the file-safety checks
//! (`docs/specs/harness/file-safety.md` §2/§3).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Serialize;

use crate::sniff::is_probably_binary;

mod cache;

pub use cache::ContentCache;

/// Bytes read from the head of a file; enough for every signature below.
pub const MAX_SNIFF_BYTES: usize = 8192;

/// Files shorter than this carry too little signal to be classified.
pub const MIN_SNIFF_BYTES: usize = 8;

/// Cache capacity in entries.
pub const CACHE_CAPACITY: usize = 512;

/// Content kinds that carry no safety signal.
pub const KIND_UNKNOWN: &str = "unknown";

/// What the bytes of a file look like, independent of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContentKind {
    Unknown,
    Text,
    Binary,
    Pe,
    Elf,
    MachO,
    Script,
    Archive,
    Image,
    Media,
    Document,
}

impl ContentKind {
    /// Stable id used by the UI and by the search filter.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Unknown => KIND_UNKNOWN,
            Self::Text => "text",
            Self::Binary => "binary",
            Self::Pe => "pe",
            Self::Elf => "elf",
            Self::MachO => "macho",
            Self::Script => "script",
            Self::Archive => "archive",
            Self::Image => "image",
            Self::Media => "media",
            Self::Document => "document",
        }
    }

    /// English label shown next to the file.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown data",
            Self::Text => "Text",
            Self::Binary => "Binary data",
            Self::Pe => "Windows executable",
            Self::Elf => "Linux executable",
            Self::MachO => "macOS executable",
            Self::Script => "Script",
            Self::Archive => "Archive",
            Self::Image => "Image",
            Self::Media => "Audio or video",
            Self::Document => "Document",
        }
    }
}

/// Result of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentReport {
    /// Detected kind.
    pub kind: ContentKind,
    /// English label of [`ContentReport::kind`].
    pub label: String,
    /// Set when the content does not match the extension.
    pub warning: Option<String>,
    /// Extension group the name claims (`text`, `executable`, `other`, `none`).
    pub claimed: String,
}

/// Extension group a name claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claimed {
    Text,
    Executable,
    Other,
    None,
}

impl Claimed {
    fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Executable => "executable",
            Self::Other => "other",
            Self::None => "none",
        }
    }
}

const TEXT_EXTENSIONS: [&str; 44] = [
    "txt", "md", "markdown", "rst", "json", "jsonl", "yaml", "yml", "toml", "ini", "cfg", "conf",
    "csv", "tsv", "log", "xml", "html", "htm", "css", "scss", "less", "js", "mjs", "cjs", "ts",
    "tsx", "jsx", "rs", "go", "java", "kt", "rb", "php", "c", "h", "cc", "cpp", "hpp", "cs",
    "swift", "sql", "tex", "diff", "env",
];

const EXECUTABLE_EXTENSIONS: [&str; 13] = [
    "exe", "dll", "so", "dylib", "sys", "bat", "cmd", "ps1", "sh", "py", "com", "scr", "msi",
];

fn lower_extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .unwrap_or_default()
}

/// Which group the file name claims, from its extension.
#[must_use]
pub fn claimed_group(path: &Path) -> Claimed {
    let ext = lower_extension(path);
    if ext.is_empty() {
        return Claimed::None;
    }
    if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        Claimed::Text
    } else if EXECUTABLE_EXTENSIONS.contains(&ext.as_str()) {
        Claimed::Executable
    } else {
        Claimed::Other
    }
}

fn starts_with(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && &bytes[..prefix.len()] == prefix
}

/// Offset of the PE header, when the buffer is a Windows image.
fn pe_header_offset(bytes: &[u8]) -> Option<usize> {
    if !starts_with(bytes, b"MZ") || bytes.len() < 0x40 {
        return None;
    }
    let raw = bytes.get(0x3c..0x40)?;
    let offset = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    let offset = usize::try_from(offset).ok()?;
    (offset + 4 <= bytes.len() && starts_with(bytes.get(offset..)?, b"PE\0\0")).then_some(offset)
}

/// Classify the first bytes of a file, ignoring the name, with the magic-byte detector.
#[must_use]
pub fn classify_magic(bytes: &[u8]) -> ContentKind {
    if bytes.len() < MIN_SNIFF_BYTES {
        return ContentKind::Unknown;
    }
    if pe_header_offset(bytes).is_some() {
        return ContentKind::Pe;
    }
    if starts_with(bytes, b"\x7fELF") {
        return ContentKind::Elf;
    }
    for magic in [
        &[0xfe, 0xed, 0xfa, 0xce][..],
        &[0xce, 0xfa, 0xed, 0xfe],
        &[0xfe, 0xed, 0xfa, 0xcf],
        &[0xcf, 0xfa, 0xed, 0xfe],
        &[0xca, 0xfe, 0xba, 0xbe],
    ] {
        if starts_with(bytes, magic) {
            return ContentKind::MachO;
        }
    }
    if starts_with(bytes, b"#!") {
        return ContentKind::Script;
    }
    if starts_with(bytes, b"%PDF") || starts_with(bytes, b"%!PS") {
        return ContentKind::Document;
    }
    if let Some(kind) = container_kind(bytes) {
        return kind;
    }
    if is_probably_binary(bytes) {
        ContentKind::Binary
    } else {
        ContentKind::Text
    }
}

/// Classify the first bytes of a file, ignoring the name.
#[cfg(not(feature = "magika"))]
#[must_use]
pub fn classify(bytes: &[u8]) -> ContentKind {
    classify_magic(bytes)
}

/// Classify the first bytes with the Magika model, falling back to magic bytes.
#[cfg(feature = "magika")]
#[must_use]
pub fn classify(bytes: &[u8]) -> ContentKind {
    ml::classify_ml(bytes)
}

fn container_kind(bytes: &[u8]) -> Option<ContentKind> {
    const ARCHIVE_MAGIC: [&[u8]; 8] = [
        b"PK\x03\x04",
        b"PK\x05\x06",
        b"\x1f\x8b",
        b"BZh",
        b"\xfd7zXZ",
        b"\x28\xb5\x2f\xfd",
        b"7z\xbc\xaf",
        b"Rar!",
    ];
    const IMAGE_MAGIC: [&[u8]; 7] = [
        b"\x89PNG",
        b"\xff\xd8\xff",
        b"GIF8",
        b"BM",
        b"II*\0",
        b"MM\0*",
        b"\0\0\x01\0",
    ];
    if ARCHIVE_MAGIC.iter().any(|magic| starts_with(bytes, magic)) {
        return Some(ContentKind::Archive);
    }
    if bytes.len() > 262 && starts_with(bytes.get(257..)?, b"ustar") {
        return Some(ContentKind::Archive);
    }
    if IMAGE_MAGIC.iter().any(|magic| starts_with(bytes, magic)) {
        return Some(ContentKind::Image);
    }
    if starts_with(bytes, b"RIFF") {
        return Some(match bytes.get(8..12) {
            Some(b"WEBP") => ContentKind::Image,
            Some(b"WAVE" | b"AVI ") => ContentKind::Media,
            _ => ContentKind::Binary,
        });
    }
    if starts_with(bytes, b"ID3") || starts_with(bytes, b"OggS") || starts_with(bytes, b"fLaC") {
        return Some(ContentKind::Media);
    }
    if bytes.get(4..8) == Some(b"ftyp") || starts_with(bytes, b"\x1aE\xdf\xa3") {
        return Some(ContentKind::Media);
    }
    None
}

/// Whether content looks wrong for the name, per the matrix in the spec §3.2.
#[must_use]
pub fn mismatch(kind: ContentKind, claimed: Claimed) -> bool {
    match kind {
        ContentKind::Unknown => false,
        ContentKind::Pe | ContentKind::Elf | ContentKind::MachO => claimed != Claimed::Executable,
        ContentKind::Script => matches!(claimed, Claimed::Text | Claimed::Other),
        ContentKind::Archive | ContentKind::Image | ContentKind::Media | ContentKind::Document => {
            claimed != Claimed::Other
        }
        ContentKind::Text => claimed == Claimed::Executable,
        ContentKind::Binary => matches!(claimed, Claimed::Text | Claimed::None),
    }
}

/// Warning text for a report, or `None` when the name fits the content.
#[must_use]
pub fn warning_for(path: &Path, kind: ContentKind) -> Option<String> {
    mismatch(kind, claimed_group(path)).then(|| format!("Content looks like: {}", kind.label()))
}

/// Build the report for one classified buffer and name.
#[must_use]
pub fn report(path: &Path, kind: ContentKind) -> ContentReport {
    let claimed = claimed_group(path);
    ContentReport {
        kind,
        label: kind.label().to_owned(),
        warning: warning_for(path, kind),
        claimed: claimed.id().to_owned(),
    }
}

fn sniff(path: &Path) -> ContentKind {
    use std::io::Read;

    let Ok(meta) = fs::metadata(path) else {
        return ContentKind::Unknown;
    };
    if !meta.is_file() {
        return ContentKind::Unknown;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return ContentKind::Unknown;
    };
    let mut head = vec![0_u8; MAX_SNIFF_BYTES];
    let mut len = 0;
    while len < head.len() {
        match file.read(&mut head[len..]) {
            Ok(0) => break,
            Ok(n) => len += n,
            Err(_) => return ContentKind::Unknown,
        }
    }
    head.truncate(len);
    classify(&head)
}

/// Detect the content kind of a file and compare it with its name.
#[must_use]
pub fn inspect(path: &Path) -> ContentReport {
    report(path, sniff(path))
}

#[cfg(feature = "magika")]
mod ml {
    use std::sync::{Mutex, OnceLock};

    use super::{classify_magic, ContentKind};

    /// Lazily built Magika session; the model load costs ~20 ms and happens on first use.
    pub fn session() -> Option<&'static Mutex<magika::Session>> {
        static SESSION: OnceLock<Option<Mutex<magika::Session>>> = OnceLock::new();
        SESSION
            .get_or_init(|| magika::Session::new().ok().map(Mutex::new))
            .as_ref()
    }

    /// Content kind from the ML model, falling back to the magic-byte detector.
    pub fn classify_ml(bytes: &[u8]) -> ContentKind {
        let Some(session) = session() else {
            return classify_magic(bytes);
        };
        let Ok(mut session) = session.lock() else {
            return classify_magic(bytes);
        };
        match session.identify_content_sync(bytes) {
            Ok(result) => map_label(result.info().label),
            Err(_) => classify_magic(bytes),
        }
    }

    pub fn map_label(label: &str) -> ContentKind {
        match label {
            "pebin" => ContentKind::Pe,
            "elf" => ContentKind::Elf,
            "macho" => ContentKind::MachO,
            "shell" | "powershell" | "python" | "batch" | "javascript" | "perl" | "ruby" => {
                ContentKind::Script
            }
            "zip" | "gzip" | "bzip" | "xz" | "sevenzip" | "rar" | "tar" | "jar" => {
                ContentKind::Archive
            }
            "png" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "ico" | "svg" => ContentKind::Image,
            "mp3" | "ogg" | "flac" | "wav" | "mp4" | "mkv" | "webm" | "flv" | "qt" => {
                ContentKind::Media
            }
            "pdf" | "postscript" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt"
            | "ods" | "odp" | "epub" | "rtf" => ContentKind::Document,
            "unknown" | "empty" => ContentKind::Unknown,
            _ => ContentKind::Text,
        }
    }
}

#[cfg(feature = "magika")]
pub use ml::{classify_ml, map_label};

#[cfg(test)]
#[path = "filetype_tests.rs"]
mod tests;
