# Work edition

Status: W0 spike done (2026-11-22), W1 spec complete.

Harness helps the user produce office work: slide decks, spreadsheets, documents and charts. These "Work items" are created by agents in any chat, previewed natively next to the chat, and exported as .pptx, .xlsx, .docx and .pdf.

Hard rule: **no webview**. Everything renders natively in Slint (PNG/SVG from Rust engines). Web-based tools may only appear as export targets opened in the user's own browser.

## 1. Architecture & Folder Layout

Work items live in `work/<slug>/` under the project root:
```
work/<slug>/
├── deck.json | book.json | doc.md | chart.json   # exactly one primary source
├── assets/                                        # images, logos, media
├── exports/                                       # generated .pptx, .xlsx, .docx, .pdf
└── .revisions/                                    # snapshots for undo/diff
```

- **Single-writer rule:** The source file on disk is the ground truth. While an agent turn edits a work item, manual edits in the side panel are queued and applied sequentially after the turn completes.
- **Engines:**
  - Layout & Document Rendering: **Typst** (Rust native, no webview).
  - Spreadsheet engine: **IronCalc** (evaluates formulas in memory, generates .xlsx).
  - PowerPoint export: Built-in OOXML writer (`zip` + DrawingML/PresentationML).
  - Word export: `docx-rs` with heading styles and tables; Pandoc used if on PATH.

## 2. Source Formats

### 2.1 Slide Decks (`deck.json`)
Identified by `"format": "harness-deck/1"`.
- `theme`: `{ "font": "Inter", "background": "#0f172a", "foreground": "#f8fafc", "accent": "#38bdf8" }`
- `slides`: array of slide objects, each having `id`, `layout`, and layout-specific content.
  - Layouts: `title`, `section`, `bullets`, `two-column`, `image`, `image-text`, `table`, `chart`, `quote`, `big-number`, `free`.
  - Elements have stable IDs and support text formatting runs (`bold`, `italic`, `code`).
  - `notes`: speaker notes string per slide.

### 2.2 Spreadsheets (`book.json`)
Identified by `"format": "harness-book/1"`.
- `sheets`: array of sheets, each with `name`, `grid` (cells keyed by coordinate e.g. `"A1"`: `{ "value": 100, "formula": "=SUM(B1:B10)", "format": "$#,##0" }`), column `widths`, row `heights`, `merges`, and `frozen_panes`.

### 2.3 Documents (`doc.md`)
Standard Markdown extended with YAML frontmatter:
```markdown
---
title: Proposal
paper: a4
margins: 2.5cm
numbering: true
---
```
Extensions:
- Page breaks: `<!-- pagebreak -->`
- Callouts: `> [!NOTE]` / `> [!WARNING]` / `> [!TIP]`
- Figures with IDs: `![Caption](path) {#fig-id}`
- Charts: fenced code block ````chart { ...json... }````
- Footnotes: `[^1]` and `[^1]: Note text`

### 2.4 Charts (`chart.json`)
Chart definition (can stand alone or embed in deck/book/doc):
- Types: `bar`, `column`, `line`, `area`, `pie`, `donut`, `scatter`, `combo`.
- Categories and series: `series: [{ "name": "Q1", "values": [10, 20, 30] }]`.

## 3. Element Paths

Element paths uniquely identify any node inside a Work item for validation errors and UI comments:
- Decks: `deck.json#/slides/2/title`, `deck.json#/slides/0/elements/header`
- Sheets: `book.json#Sheet1!A1:B10`, `book.json#Financials!C5`
- Docs: `doc.md#intro`, `doc.md#/blocks/3`

## 4. UI Components

1. **Chat Card (`WorkCard`):** In transcript when an item is created or edited. Displays thumbnail/icon, title, kind/size, agent, and buttons (`Open`, `Export`, `Present`).
2. **Side Panel (`WorkPanel` + `WorkSheetView`):** Split view beside chat (Work tab in side pane).
   - Decks: Slide thumbnails strip, main canvas, speaker notes, warnings banner, present mode (`WorkPresent`), comment-to-composer input.
   - Sheets: Native Slint grid (`WorkSheetView`), formula bar with inline editing, sheet tabs, error highlighting (`#REF!`, `#DIV/0!`, etc.).
   - Docs & Charts: Paginated Typst view, page thumbnails, comment input.
3. **Work Library Page (`WorkPage`):** Top-bar navigation beside PRD and Orchestra. Filterable & searchable list of workspace Work items, 6 built-in templates, and Office file import (`.pptx`, `.xlsx`, `.csv`, `.docx`, `.md`).
4. **Composer `+` Menu:** Quick starters for Slides, Spreadsheet, Document, and opening Work Library.

## 5. Non-goals, Fidelity Limits & Acceptance Scenarios

- **Non-goals:** No embedded webview; no WYSIWYG drag-and-drop slide vector editor; no macro/VBA execution; no cloud storage sync; no automatic installation of Pandoc or LibreOffice.
- **Fidelity limits:**
  - `.pptx` export covers text runs, bullets, images, tables, native DrawingML charts, themes, and footers; animations/transitions and SmartArt are not emitted.
  - `.xlsx` / `.csv` formulas are evaluated by IronCalc (~500 functions); upstream off-by-one `EOMONTH` bug noted below.
  - `.docx` export covers headings, paragraphs, lists, tables, images, callouts, and footnotes via `docx-rs` (Pandoc used when present on PATH).
  - Imports preserve original source files under `assets/original/` and report dropped elements (animations, macros, pivots) in `ImportReport.warnings`.
- **Acceptance scenarios:**
  1. Create a deck from template or agent `work_create` → validates cleanly → renders PNG slides → opens in side panel → presents full-window with arrow keys/Esc → exports `.pptx` & `.pdf`.
  2. Create/import a spreadsheet → formulas evaluate live in `WorkSheetView` → inline cell edit saves `.revisions/` snapshot and recalculates → exports `.xlsx` & `.csv`.
  3. Add a comment on an open slide/cell/page → attaches as composer chip with `ContextItem::WorkItem` → agent receives Work instructions + element path + comment.

## 6. Spike result (W0)

The spike was a throwaway crate outside the repo (`D:\work-spike`, release profile, Windows 10, rustc 1.99.1). Office files were checked by opening them in hidden Microsoft Office 16 through COM. LibreOffice and Google Slides were not available, so they were not checked.

### Typst 0.15.1 — layout, preview, PDF

- License: Apache-2.0. MSRV 1.92.
- Embedding: own `World` (about 40 lines) plus `typst-kit` fonts, `typst-render` (PNG), `typst-pdf`.
- Speed on a 2-slide deck (16:9, table, bullets) and an A4 page (headings, table, figure caption):

  | Step | Time |
  |---|---|
  | Compile, cold | 5–6 ms |
  | Compile, unchanged source (memoized) | under 0.05 ms |
  | PNG at 1 px/pt | about 4 ms per slide, 6.5–10 ms per A4 page |
  | PDF | 0.7–1.9 ms |
  | System font scan | 26–140 ms (once, on a worker) |

  The target was under 150 ms per slide, so Typst is far within it.
- Quality: clean typesetting with system fonts (Segoe UI), tables, lists, headers and footers, page numbers, and figure captions.
- **Size:** the spike exe was 50.9 MB with Typst's embedded fonts (9.6 MB) and 41.2 MB without them. The bulk is ICU data, syntax highlighting and bibliography support, which Typst does not make optional. `harness-slint.exe` is 122 MB today. The real growth can only be measured inside `harness-slint` (some dependencies are shared), and this is done in W2.
  - Decision: do not embed Typst's font set. Use system fonts plus one or two bundled open fonts (about 1–2 MB) so layout is the same on every machine.

### IronCalc 0.9.1 — sheets

- License: MIT OR Apache-2.0. The spike exe was 6.8 MB.
- Formula check: 14 of 15 business formulas were right:
  - correct: SUM, AVERAGE, IF, ROUND, VLOOKUP, XLOOKUP, SUMIFS, COUNTIFS, TEXT, string functions, INDEX/MATCH, FILTER (dynamic arrays), and the errors `#DIV/0!` and `#NAME?`;
  - wrong: `EOMONTH(D2,0)-D2` gives 15 in IronCalc where Excel gives 16 (an off-by-one date bug upstream).
  - About 500 functions are implemented.
- Speed on a 50k-cell sheet (10k rows of 4 values plus 1 formula): input 88 ms, first evaluate 5 ms, recalculation after one edit 3.4 ms. The target was under 200 ms.
- .xlsx: `save_to_xlsx` output opened in Excel with every formula kept and the same results; IronCalc load and save round trip is fine.
- Decision: **IronCalc**. The calamine and rust_xlsxwriter fallback is not needed.

### .pptx — own OOXML writer

- About 200 lines on the `zip` crate. Parts: content types, presentation, one master, one blank layout, theme, slides, a chart part, media.
- PowerPoint opened the file without a repair prompt. The table is a native table (`HasTable`), and the column chart is a native chart (`HasChart`) with theme colors, axes, gridlines and legend. The text boxes and a picture (a Typst PNG) rendered as expected.
- Gaps to close in W5:
  - Embed the chart's data workbook (IronCalc can write it) so "Edit Data" works in PowerPoint.
  - Speaker notes need a notes master and are not tested yet.
  - Check in LibreOffice and the Google Slides import.

### .docx — docx-rs 0.4.22

- License: MIT.
- Word opened the file with headings recognized as outline levels 1 and 2 and the table intact, and it exported to PDF.
- Decision: **docx-rs**, with Pandoc used when it is on PATH.

### Upstream issue to report

- IronCalc `EOMONTH` off by one (see above).
