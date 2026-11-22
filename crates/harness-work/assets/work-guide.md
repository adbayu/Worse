Harness Work items are folders `work/<slug>/` in the project with one source file you edit with your normal file tools, plus `assets/` (images, logos) and `exports/` (generated files). Create items with `work_create`, edit the source, then call `work_validate` and `work_render` to check the result before you tell the user it is done. Never write .pptx/.docx/.xlsx by hand: call `work_export`.

## Slides — `deck.json` (`"format": "harness-deck/1"`)
`{"format","title","theme":{"background","foreground","accent","muted","font","headingFont","footer","logo"},"slides":[...]}`. Canvas 1280×720 pt. Every slide has `id` and `layout`:
- `title`: `title`, `subtitle` · `section`: `title`, `subtitle`
- `bullets`: `title`, `bullets` (≤ 6, short) · `two-column`: `title`, `left`, `right` (string lists)
- `image`: `title`, `image` (`assets/x.png`), `caption` · `image-text`: `title`, `image`, `bullets`
- `table`: `title`, `table: {"header": [...], "rows": [[...]]}` (≤ 12 rows, ≤ 7 columns)
- `chart`: `title`, `chart` (see Chart) · `quote`: `quote`, `by` · `big-number`: `title`, `number`, `label`
- `free`: `title`, `elements: [{"id","kind":"text|image|shape|table|chart","x","y","w","h","text","size","image","fill","color","table","chart"}]`
Any slide may have `notes` (speaker notes). Text supports `**bold**`, `*italic*` and `` `code` ``. One idea per slide; split long slides instead of shrinking text.

## Sheets — `book.json` (`"format": "harness-book/1"`)
`{"format","title","sheets":[{"name","cells":{"A1":"Region","B2":1200,"B9":"=SUM(B2:B8)"},"formats":{"B2:B9":"#,##0"},"widths":{"A":20},"fills":{"A1:D1":"#e5e7eb"},"bold":["A1:D1"],"merges":["A1:D1"],"freeze":"A2","conditional":[{"range":"D2:D9","when":"<0","fill":"#fee2e2"}],"charts":[{"id","anchor":"F2","chart":{...,"data":{"range":"Sheet1!A1:C9"}}}]}],"names":{"Total":"Sheet1!$B$9"}}`.
Formulas start with `=` and are computed by the engine (Excel functions). Use `work_sheet` for overview, `find` (`exact`, `sheet`, `in`), `read` (`values`, `formulas`, `pairs`), `set`, `fill`, `insert_rows`/`delete_rows`/`insert_cols`/`delete_cols`, `style`, `errors`, `trace`, and `diff`; never compute totals yourself, and never read or grep `book.json` or `style.json` directly. Imported `.xlsx` workbooks export in write-back mode by default (preserving charts, pivots, macros, styles): cell values and `style` edits are written back, but widths, heights, merges, freeze, hidden rows/cols, conditional formats, validation, links and notes only change with `clean: true` on `work_export` (a clean rebuild).

## Docs — `doc.md`
Markdown with frontmatter (`title`, `paper: a4|letter`, `margin` cm, `header`, `footer`, `cover: true`, `numbering: true`, `toc: true`, `font`). Supports headings, lists, tables, images `![caption](assets/x.png){#id}`, footnotes `[^1]`, callouts `> [!NOTE]`, `<!-- pagebreak -->`, and charts in a ```` ```chart ```` fence holding Chart JSON.

## Charts — `chart.json` (`"format": "harness-chart/1"`) or inline
`{"type":"bar|column|line|area|pie|donut|scatter|combo","title","categories":[...],"series":[{"name","values":[...],"type":"line"}],"stacked":false,"xLabel","yLabel","legend":true}` or `"data":{"range":"Sheet1!A1:C9","book":"../budget/book.json"}` (row 1 = series names, column A = categories).

## Element paths
Issues, comments and hit boxes name parts as `deck.json#/slides/2/title`, `book.json#Sheet1!B2:D9`, `doc.md#pricing/3`. When the user's message carries such a path, change exactly that part.

## Brand
`work/brand.json` (`work_brand`) sets colors, fonts, logo and footer for new items and exports.
