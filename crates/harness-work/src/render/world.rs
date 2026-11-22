//! The Typst `World` for one compile: one generated main source, files read from the item folder
//! (never outside it), and virtual files (chart SVGs) keyed by name. Fonts are loaded once per
//! process: system fonts plus the bundled open fonts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime};
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook, FontInfo};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::FontStore;

const BUNDLED: [&[u8]; 3] = [
    include_bytes!("../../assets/fonts/LibertinusSerif-Regular.otf"),
    include_bytes!("../../assets/fonts/LibertinusSerif-Bold.otf"),
    include_bytes!("../../assets/fonts/DejaVuSansMono.ttf"),
];

/// Fonts are scanned once (26–140 ms) and shared by every compile.
fn fonts() -> &'static FontStore {
    static FONTS: OnceLock<FontStore> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut store = FontStore::new();
        for data in BUNDLED {
            for font in Font::iter(Bytes::new(data)) {
                let info: FontInfo = font.info().clone();
                store.push((font, info));
            }
        }
        store.extend(typst_kit::fonts::system());
        store
    })
}

fn library() -> &'static LazyHash<Library> {
    static LIB: OnceLock<LazyHash<Library>> = OnceLock::new();
    LIB.get_or_init(|| LazyHash::new(Library::default()))
}

/// Warms the font cache on a worker so the first preview is fast.
pub fn warm_up() {
    let _ = fonts();
}

pub(crate) struct WorkWorld {
    source: Source,
    root: PathBuf,
    virtual_files: HashMap<String, Bytes>,
}

impl WorkWorld {
    pub(crate) fn new(
        text: String,
        root: PathBuf,
        virtual_files: HashMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            source: Source::detached(text),
            root,
            virtual_files: virtual_files
                .into_iter()
                .map(|(k, v)| (k, Bytes::new(v)))
                .collect(),
        }
    }
}

impl World for WorkWorld {
    fn library(&self) -> &LazyHash<Library> {
        library()
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts().book()
    }

    fn main(&self) -> FileId {
        self.source.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.source.id() {
            Ok(self.source.clone())
        } else {
            Err(FileError::AccessDenied)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let rel = id.vpath().get_without_slash().replace('\\', "/");
        if let Some(bytes) = self.virtual_files.get(&rel) {
            return Ok(bytes.clone());
        }
        if rel.split('/').any(|part| part == "..")
            || !matches!(id.root(), typst::syntax::VirtualRoot::Project)
        {
            return Err(FileError::AccessDenied);
        }
        let path = self.root.join(&rel);
        std::fs::read(&path)
            .map(Bytes::new)
            .map_err(|e| FileError::from_io(e, &path))
    }

    fn font(&self, index: usize) -> Option<Font> {
        fonts().font(index)
    }

    fn today(&self, _offset: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}
