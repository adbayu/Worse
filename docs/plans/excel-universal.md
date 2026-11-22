# Plan: universal Excel support in Work (sheets)

Status: implemented (Phases 1–5, 2026-11-22). Scope: `crates/harness-work` (import, `book.json` +
`style.json`, `SheetModel`, `work_sheet` MCP tool, write-back export), `crates/harness-api-agent`
(large-book guard), the sheet grid in `apps/harness-slint/src/bridge/work/` + `work_sheet.slint`,
and the Work prompt in `crates/harness-agent-hub/src/work.rs`. No webview.

## 1. Why

An agent spent ~8 tool calls and tens of thousands of tokens to change one number in an imported
workbook (`work/airfast-loadsheet`). Root causes found:

| Finding | Evidence |
| --- | --- |
| `work_sheet` could not list sheets, search, or read a bare sheet name (`read: "*"` → "bad range") | fixed on 2026-11-22 (`mcp/sheet.rs`: overview, `find`, sheet-name read, compact `H5=30000 (=formula)` output) |
| The agent fell back to `read_file`/grep on `book.json` | prompt now forbids it; nothing enforces it |
| `book.json` is mostly style noise: 5,799 cell entries vs ~35,000 style entries (borders 13,403, sizes 10,780, align 4,939, formats 2,620, bold 1,943, fills 1,069); 978 KB | importer writes styles per cell, never as ranges |
| Every `work_sheet` call rebuilds the IronCalc model from that JSON, including all styles | `SheetModel::from_book` (`sheet.rs:59`) |
| Defined names are never imported, so formulas that use them give `#NAME?` | `import/sheet.rs` does not read `model.workbook.defined_names`; `Book.names` exists |
| No check that our recalculated values match what Excel computed | nothing compares against the cached `<v>` values in the xlsx |

The formula engine itself is not the bottleneck: IronCalc 0.9.1 implements 496 functions,
covering the commonly used set (SUM, IF, IFS, IFERROR, AND/OR, SUMIF(S), COUNTIF(S),
AVERAGEIF(S), VLOOKUP/HLOOKUP/XLOOKUP, INDEX/MATCH/XMATCH, CHOOSE, OFFSET, INDIRECT, ROUND*,
CEILING/FLOOR, SUMPRODUCT, MIN/MAXIFS, TEXT/VALUE, LEFT/RIGHT/MID/LEN/TRIM/SUBSTITUTE,
TEXTJOIN, TEXTBEFORE/AFTER/SPLIT, DATE/EOMONTH/EDATE/DATEDIF/NETWORKDAYS/WORKDAY, PMT/NPV/IRR,
FILTER/SORT/UNIQUE/SEQUENCE, LET/LAMBDA, SUBTOTAL). Checked by name; RANK/VAR/AGGREGATE exist
only as modern variants or not at all and need a runtime check (phase 1).

## 2. What "typical Excel" means (target feature list)

From common usage guides and Microsoft's own "most popular functions" list: SUM, IF, LOOKUP,
VLOOKUP, XLOOKUP, INDEX/MATCH, SUMIFS/COUNTIFS, IFERROR, AVERAGE, CONCAT/TEXT, date functions.
Workbook features real business files use, in priority order:

1. Values, formulas, cross-sheet references (`'Sheet 2'!A1`), absolute/relative refs.
2. Number formats (currency, %, dates, thousands separators), dates as serials.
3. Merges, column widths, **row heights**, **hidden rows/columns**, hidden sheets, frozen panes.
4. Styles: fills, fonts (bold/italic/color/size), alignment, wrap, **border colors**.
5. **Defined names** (workbook and sheet scope).
6. **Excel Tables** (ListObjects) and structured references (`Table1[Amount]`).
7. **Data validation** (dropdown lists), **conditional formatting** (cell rules, color scales).
8. Comments/notes, hyperlinks.
9. Charts, pivot tables, images, macros (keep in the original; do not rebuild).
10. External links (`[1]Sheet!A1`), iterative calc / circular references, dynamic-array spills.

## 3. Plan

### Phase 1: correctness audit at import (highest value, small)

- Read each formula cell's cached value (`<v>` in `xl/worksheets/sheetN.xml`) while importing,
  recompute with IronCalc, and record mismatches: `{cell, excel, ours, formula}`.
- Write a short report into the import warnings and `work/<slug>/import-report.md`: unknown
  functions (`#NAME?`), value mismatches, external links, circular references.
- Import defined names into `Book.names` (workbook scope; sheet scope as `Sheet!Name`).
- Add a fixture test set of real-world shaped workbooks (loadsheet, budget, invoice, schedule with
  dates, lookup table, named ranges, table with structured refs) under
  `crates/harness-work/tests/fixtures/` with expected values.

Done when: importing a workbook tells the user exactly which cells differ from Excel and why.

### Phase 2: lean source format (token + speed)

- Store styles as ranges, not per cell: merge equal adjacent cells into rectangles
  (row runs, then stack equal runs vertically). Expect 10–30× fewer style entries.
- Move presentation into `style.json` next to `book.json` (fills, borders, align, sizes, colors,
  bold, widths, heights, merges, hidden rows/cols). `book.json` keeps cells, formats, names,
  conditional, charts. Old files keep loading (fields still accepted in `book.json`).
- Cache the evaluated `SheetModel` per item in the MCP server keyed by file mtime; `set` updates
  the cached model with `set_user_input` instead of rebuilding.

Done when: airfast `book.json` < 250 KB and a `work_sheet` call on it takes < 100 ms.

### Phase 3: agent tool set (`work_sheet` and friends)

Following the discover → profile → extract pattern (spreadsheet-mcp) and SpreadsheetLLM's
findings (skip empty/uniform cells, index by value, group by format: ~96% fewer tokens):

- `overview`: per sheet, the used range, detected table blocks (header row + extent) and label
  cells, not just sheet names. (Have: names + used range.)
- `find` (have): add `exact`, `sheet` and `in: values|formulas|labels` filters.
- `read` (have): add `mode: values|formulas` and a header-aware mode that prints a block as
  `label → value` pairs for form-style sheets like loadsheets.
- `trace`: precedents and dependents of a cell, so "why is H8 NULL" needs one call.
- `errors`: list every error cell (`#VALUE!`, `#REF!`, `#NAME?`, `#DIV/0!`) with its formula and
  first failing input.
- `fill`: write one formula into a range with relative-reference adjustment (no per-cell `set`).
- `rows`/`cols`: insert/delete with reference shifting (IronCalc `insert_rows`/`delete_rows`).
- `style`: set format/fill/bold/border on a range through the tool.
- `diff`: cell changes since import or since the turn started (feeds the "1 file changed" card).
- Enforcement: the API agent's `read_file`/`search`/`edit_file` on `work/*/book.json` larger
  than ~64 KB return a short hint pointing at `work_sheet` instead of the content
  (`crates/harness-api-agent/src/tools.rs`). External CLIs (Claude Code, Codex) get the prompt rule.

### Phase 4: fidelity of import, grid and export

- Import: row heights, hidden rows/columns, italic/underline/wrap, border colors, data validation
  lists, Excel conditional-formatting rules (cell value rules and color scales first), comments,
  hyperlinks, tables → named ranges + structured-ref rewrite (until IronCalc supports them).
- Grid (Slint): variable row heights, hidden rows/cols, border colors, wrap, a dropdown for
  validation lists, an error tooltip.
- Export: write-back mode. Keep `assets/original/*.xlsx` as the base and patch only changed cells
  (and their cached values) into its XML, so charts, pivots, macros, images and anything we do not
  model survive. The current full re-write via IronCalc stays as "export clean copy".

### Phase 5: locale and calc details

- Date system 1904, workbook locale for display (decimal comma for id-ID), volatile functions
  (TODAY/NOW/RAND recalc on open), iterative calculation flag, dynamic-array spill display.

## 4. Order and size

| Phase | Size | Depends on |
| --- | --- | --- |
| 1 Audit + defined names + fixtures | S–M | – |
| 2 Lean format + model cache | M | – |
| 3 Tool set + enforcement | M | 2 (cache) |
| 4 Fidelity + write-back export | L | 1 |
| 5 Locale/calc details | S | 1 |

Recommended start: 1 and 2 together (they explain and remove most of the waste), then 3.

## Sources

- SpreadsheetLLM (Microsoft, EMNLP 2024): https://arxiv.org/abs/2407.09025
- spreadsheet-mcp README: https://docs.rs/crate/spreadsheet-mcp/latest/source/README.md
- SpreadsheetBench 2: https://arxiv.org/pdf/2606.29955
- Microsoft, Excel functions by category: https://support.microsoft.com/en-us/excel/excel-functions-by-category
- IronCalc: https://github.com/ironcalc
