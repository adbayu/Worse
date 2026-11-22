//! Work edition: instructions injected into turns carrying `ContextItem::WorkItem`.
//! Agents create, validate, render, and export office work items using the `harness-work` tools.

pub(crate) const LABEL: &str = "Work";

pub(crate) const INSTRUCTIONS: &str = r#"<work-edition>
You are working on an office Work item (slide deck, spreadsheet, document, or chart).
The user sees your Work items live in the side panel and as cards in chat. The "Active work item
path" below is the item open in that panel: when the user talks about the Excel, Word or
PowerPoint file they have open, they mean it, not a desktop Office window. Read it with
`work_sheet` (sheets) or `work_render` before answering. Follow this loop:

1. Discover & Plan:
- If purpose, audience, length, or style is unclear, ask clarifying questions before writing.
- For decks and docs, formulate an outline first before writing all slides or sections.

2. Create & Edit:
- Create items using the `work_create` tool or by writing source files in `work/<slug>/`:
  - `deck.json`: theme, slides with layouts (`title`, `section`, `bullets`, `two-column`, `image`, `table`, `chart`, `quote`, `big-number`), speaker notes.
  - `book.json`: sheets, cell values and standard formulas (`=SUM()`, `=AVERAGE()`, `=IF()`, etc.), formats.
  - `doc.md`: Markdown with YAML frontmatter, headers, tables, callouts (`> [!NOTE]`), page breaks (`<!-- pagebreak -->`).
  - `chart.json`: bar, column, line, area, pie, donut, scatter, combo.

- Existing sheets (above all imported Excel files): never read, grep or edit `book.json` or
  `style.json` directly, they can be huge. Use `work_sheet`: no args for overview (sheets + table
  blocks), `find` (`exact`, `sheet`, `in: values|formulas|labels`), `read` (`mode: values|formulas|pairs`),
  `set`, `fill` (copy formula with relative refs), `insert_rows`/`delete_rows`/`insert_cols`/`delete_cols`,
  `style`, `errors`, `trace` (precedents/dependents), and `diff`. `set` returns the recalculated
  values; that is enough to confirm a value change, no `work_validate`/`work_render` needed.

3. The "Look" Loop (Verify & Refine):
- Call `work_validate` to check for syntax, schema, or missing asset errors.
- Call `work_render` to render slides or pages to PNG and inspect the layout visually.
- Fix text overflow, awkward wrapping, or misplaced elements once, then confirm.

4. Export:
- Only export when asked, using `work_export` (.pptx, .xlsx, .docx, .pdf).
</work-edition>"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use harness_core_api::ContextItem;

    #[test]
    fn work_item_wire_format() {
        let item = ContextItem::WorkItem {
            path: "work/pitch".into(),
        };
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type": "workItem", "path": "work/pitch"})
        );
    }

    #[test]
    fn work_helper_uses_work_mcp_flag() {
        let launch = crate::helpers::HelperLaunch::SelfExe {
            program: std::path::PathBuf::from("harness-slint"),
        };
        assert_eq!(
            launch.args(crate::helpers::Helper::Work),
            vec![crate::helpers::FLAG_WORK_MCP]
        );
        assert!(super::INSTRUCTIONS.contains("work_validate"));
        assert!(super::INSTRUCTIONS.contains("work_render"));
    }
}
