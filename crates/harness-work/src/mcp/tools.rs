//! Tool definitions and handlers of the `harness-work` MCP server.

use std::fmt::Write as _;

use base64::Engine as _;
use serde_json::{json, Value};

use super::{arg, failure, text, Server};
use crate::brand::{load_brand, save_brand, Brand};
use crate::create::{create, templates};
use crate::export::{export, export_clean, ExportFormat};
use crate::import::import;
use crate::item::Kind;
use crate::render::{render, Target};
use crate::scan::scan;
use crate::validate::{validate, Severity};
use crate::WORK_DIR;

pub(crate) const INSTRUCTIONS: &str = include_str!("../../assets/work-guide.md");

/// Pages returned as images by one `work_render` call.
const MAX_PAGES: usize = 6;

#[allow(clippy::needless_pass_by_value)]
fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": props, "required": required}})
}

pub(crate) fn definitions() -> Value {
    let path = json!({"type": "string", "description": "Item folder, its source file, or its slug under work/"});
    json!([
        tool("work_guide", "The Work source formats (slides, sheets, docs, charts) and element paths. Read once before editing a Work item.", json!({}), &[]),
        tool("work_list", "List the project's Work items (slides, sheets, docs, charts) with titles and paths.", json!({}), &[]),
        tool("work_templates", "List built-in and project templates.", json!({}), &[]),
        tool("work_create", "Create a Work item folder under work/ and return its source path. kind: slides|sheet|doc|chart.", json!({
            "kind": {"type": "string", "enum": ["slides", "sheet", "doc", "chart"]},
            "title": {"type": "string"},
            "template": {"type": "string", "description": "Template id from work_templates (optional)"}
        }), &["kind", "title"]),
        tool("work_validate", "Check a Work item: schema, missing fields, text that will not fit, tables too large, missing images, chart data. Returns issues with element paths.", json!({"path": path}), &["path"]),
        tool("work_render", "Render a Work item to images so you can see it. target: all (default), 'slide 3', 'page 2' or a sheet range 'Sheet1!A1:F20'.", json!({
            "path": path,
            "target": {"type": "string"},
            "scale": {"type": "number", "description": "Pixels per point, default 0.75"}
        }), &["path"]),
        tool("work_export", "Export a Work item to exports/: pptx, docx, xlsx, csv, pdf, png, html or svg. For imported xlsx, write-back preserves charts/pivots/macros unless clean=true.", json!({
            "path": path,
            "format": {"type": "string", "enum": ["pptx", "docx", "xlsx", "csv", "pdf", "png", "html", "svg"]},
            "clean": {"type": "boolean", "description": "For xlsx: rebuild clean file via IronCalc instead of patching the imported original"}
        }), &["path", "format"]),
        tool("work_import", "Import an Office or text file (xlsx, csv, pptx, docx, md) as a new Work item.", json!({"file": {"type": "string"}}), &["file"]),
        tool("work_sheet", "Inspect and edit spreadsheets; never read or grep book.json directly. No args: overview (sheets, used ranges, table blocks). find (+ exact, sheet, in: values|formulas|labels): search cells. read (+ mode: values|formulas|pairs): 'Sheet1!A1:H20', sheet name, or '*'. set: {\"Sheet1!B2\": \"=SUM(B3:B9)\"}. fill: {\"from\":\"Sheet!D2\",\"to\":\"D3:D20\"}. insert_rows/delete_rows/insert_cols/delete_cols: 'Sheet!5:6' or 'Sheet!C'. style: {\"Sheet!A1:D1\":{\"bold\":true,\"fill\":\"#E5E7EB\",\"format\":\"#,##0\"}}. errors: true. trace: 'Sheet!H8'. diff: true.", json!({
            "path": path,
            "find": {"type": "string"},
            "exact": {"type": "boolean"},
            "sheet": {"type": "string"},
            "in": {"type": "string", "enum": ["values", "formulas", "labels"]},
            "read": {"type": "string"},
            "mode": {"type": "string", "enum": ["values", "formulas", "pairs"]},
            "set": {"type": "object"},
            "fill": {"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}}},
            "insert_rows": {"type": "string"},
            "delete_rows": {"type": "string"},
            "insert_cols": {"type": "string"},
            "delete_cols": {"type": "string"},
            "style": {"type": "object"},
            "errors": {"type": "boolean"},
            "trace": {"type": "string"},
            "diff": {"type": "boolean"}
        }), &["path"]),
        tool("work_brand", "Read the brand kit (work/brand.json) or update fields: name, accent, background, foreground, muted, font, headingFont, logo, footer.", json!({"set": {"type": "object"}}), &[]),
    ])
}

impl Server {
    pub(crate) fn call(&self, name: &str, args: &Value) -> Value {
        let r = match name {
            "work_guide" => Ok(text(INSTRUCTIONS)),
            "work_list" => Ok(text(
                &serde_json::to_string_pretty(&scan(&self.project)).unwrap_or_default(),
            )),
            "work_templates" => Ok(text(
                &serde_json::to_string_pretty(&templates(&self.project)).unwrap_or_default(),
            )),
            "work_create" => self.create(args),
            "work_validate" => self.validate(args),
            "work_render" => self.render(args),
            "work_export" => self.export(args),
            "work_import" => import(&self.project, &self.file(arg(args, "file")))
                .map(|r| text(&serde_json::to_string_pretty(&r).unwrap_or_default()))
                .map_err(|e| e.to_string()),
            "work_sheet" => self.sheet(args),
            "work_brand" => self.brand(args),
            other => Err(format!("unknown tool {other}")),
        };
        r.unwrap_or_else(|e| failure(&e))
    }

    fn create(&self, args: &Value) -> Result<Value, String> {
        let kind =
            Kind::parse(arg(args, "kind")).ok_or("kind must be slides, sheet, doc or chart")?;
        let title = arg(args, "title");
        let item = create(
            &self.project,
            kind,
            if title.is_empty() { "Untitled" } else { title },
            arg(args, "template"),
        )
        .map_err(|e| e.to_string())?;
        Ok(text(&format!(
            "Created {} at {}\nEdit {} (see work_guide for the format), then call work_validate and work_render.",
            kind.label(),
            item.dir.display(),
            item.source_path().display()
        )))
    }

    fn validate(&self, args: &Value) -> Result<Value, String> {
        let item = self.item(arg(args, "path"))?;
        let issues = validate(&item);
        if issues.is_empty() {
            return Ok(text("No issues."));
        }
        let mut s = String::new();
        for i in &issues {
            let tag = if i.severity == Severity::Error {
                "error"
            } else {
                "warning"
            };
            let _ = writeln!(s, "{tag} {}: {}", i.path, i.message);
        }
        Ok(text(&s))
    }

    fn render(&self, args: &Value) -> Result<Value, String> {
        let item = self.item(arg(args, "path"))?;
        let target = Target::parse(arg(args, "target"));
        #[allow(clippy::cast_possible_truncation)]
        let scale = args
            .get("scale")
            .and_then(Value::as_f64)
            .unwrap_or(0.75)
            .clamp(0.25, 2.0) as f32;
        let out = render(&item, &target, scale).map_err(|e| e.to_string())?;
        let mut content = Vec::new();
        let mut note = format!(
            "{} page(s) in the item; showing {}.",
            out.page_count,
            out.pages.len().min(MAX_PAGES)
        );
        if out.pages.len() > MAX_PAGES {
            let _ = write!(note, " Ask for 'slide N' / 'page N' to see the rest.");
        }
        for w in &out.warnings {
            let _ = write!(note, "\nwarning: {w}");
        }
        content.push(json!({"type": "text", "text": note}));
        for p in out.pages.iter().take(MAX_PAGES) {
            content.push(json!({"type": "text", "text": format!("Page {}", p.index + 1)}));
            content.push(json!({"type": "image", "mimeType": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(&p.png)}));
        }
        Ok(json!({"content": content}))
    }

    fn export(&self, args: &Value) -> Result<Value, String> {
        let item = self.item(arg(args, "path"))?;
        let format = ExportFormat::parse(arg(args, "format")).ok_or("unknown format")?;
        let clean = args.get("clean").and_then(Value::as_bool) == Some(true);
        let files = if clean {
            export_clean(&item, format)
        } else {
            export(&item, format)
        }
        .map_err(|e| e.to_string())?;
        let list: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        Ok(text(&format!("Exported:\n{}", list.join("\n"))))
    }

    fn brand(&self, args: &Value) -> Result<Value, String> {
        let work = self.project.join(WORK_DIR);
        let mut brand = load_brand(&work);
        if let Some(set) = args.get("set").filter(|v| v.is_object()) {
            let mut current = serde_json::to_value(&brand).map_err(|e| e.to_string())?;
            if let (Some(obj), Some(patch)) = (current.as_object_mut(), set.as_object()) {
                for (k, v) in patch {
                    obj.insert(k.clone(), v.clone());
                }
            }
            brand = serde_json::from_value::<Brand>(current).map_err(|e| e.to_string())?;
            save_brand(&work, &brand).map_err(|e| e.to_string())?;
        }
        Ok(text(
            &serde_json::to_string_pretty(&brand).unwrap_or_default(),
        ))
    }
}
