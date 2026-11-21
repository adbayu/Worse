//! The `fact_check` tool of the API agent (docs/specs/harness/deep-research.md, section "Fact check"): the
//! tool definition and dispatch, `factcheck.json` and the LaTeX appendix table.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::factcheck_claims::{extract, record, score, Claim, CLAIM_MAX, QUERY_MAX};

pub const NAME: &str = "fact_check";
const TEXT_MAX: usize = 40_000;

static RECORDED: Mutex<Option<HashMap<String, Vec<Claim>>>> = Mutex::new(None);

fn claims_arg(args: &Value) -> Vec<Claim> {
    match args.get("claims") {
        Some(Value::String(text)) => record(serde_json::from_str::<Value>(text).ok().as_ref()),
        other => record(other),
    }
}

fn remember(question: &str, batch: Vec<Claim>) -> Vec<Claim> {
    let key = question.to_lowercase();
    let Ok(mut guard) = RECORDED.lock() else {
        return batch;
    };
    let all = guard
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .or_default();
    for claim in batch {
        let same = claim.text.to_lowercase();
        match all.iter_mut().find(|c| c.text.to_lowercase() == same) {
            Some(slot) => *slot = claim,
            None => all.push(claim),
        }
    }
    all.clone()
}

fn recorded(question: &str) -> Vec<Claim> {
    RECORDED
        .lock()
        .ok()
        .and_then(|g| {
            g.as_ref()
                .and_then(|m| m.get(&question.to_lowercase()).cloned())
        })
        .unwrap_or_default()
}

pub fn definition() -> Value {
    json!({"type": "function", "function": {
        "name": NAME,
        "description": "Fact check for a deep research report (docs/specs/harness/deep-research.md). Actions: \
            \"extract\" splits the report into atomic claims and drops method statements; \"record\" takes the \
            verdicts (supported, notSupported, unclear) with their sources and returns the score; record may be called \
            in batches with the same question and the verdicts add up; \"write\" renders factcheck.json and the LaTeX \
            appendix table from every recorded verdict, so claims can be omitted there. Verify each claim with web_search or paper_search, at most \
            three queries per claim, and cite only sources you opened.",
        "parameters": {"type": "object", "properties": {
            "action": {"type": "string", "description": "extract, record or write"},
            "question": {"type": "string", "description": "The research question"},
            "report": {"type": "string", "description": "Report text, LaTeX or Markdown; for extract and write"},
            "claims": {"type": "array", "description": "Verdicts: text, location, verdict, reason, sources [{url, title}], queries [string]"},
            "dir": {"type": "string", "description": "Report folder, for example research/<slug>; for write"}
        }, "required": ["action"]}
    }})
}

pub fn handles(name: &str) -> bool {
    name == NAME
}

pub fn title(args: &Value) -> String {
    match arg(args, "action") {
        "record" => "Score claims".to_owned(),
        "write" => "Write factcheck.json".to_owned(),
        _ => "Extract claims".to_owned(),
    }
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
}

/// Runs one `fact_check` call; the result is the text handed back to the model.
#[cfg(test)]
pub fn run(args: &Value) -> Result<String, String> {
    run_at(args, None)
}

/// `write` with a dir writes the files when the caller passes the resolved, permitted folder.
pub fn writes(args: &Value) -> bool {
    arg(args, "action") == "write" && !arg(args, "dir").is_empty()
}

pub fn run_at(args: &Value, root: Option<&std::path::Path>) -> Result<String, String> {
    let action = arg(args, "action");
    match action {
        "extract" => {
            let report = arg(args, "report");
            if report.is_empty() {
                return Err("report is empty".into());
            }
            let claims = extract(
                &report.chars().take(TEXT_MAX).collect::<String>(),
                arg(args, "question"),
            );
            Ok(render_claims(&claims))
        }
        "record" => {
            let batch = claims_arg(args);
            if batch.is_empty() {
                return Err(
                    "claims is empty or not an array of objects with text and verdict".into(),
                );
            }
            let claims = remember(arg(args, "question"), batch);
            Ok(render_scored(&claims))
        }
        "write" => {
            let batch = claims_arg(args);
            let claims = if batch.is_empty() {
                recorded(arg(args, "question"))
            } else {
                remember(arg(args, "question"), batch)
            };
            if claims.is_empty() {
                return Err("claims is empty: record the verdicts first".into());
            }
            match root {
                Some(root) => {
                    write_files(root, arg(args, "question"), arg(args, "report"), &claims)
                }
                None => Ok(render_output(
                    arg(args, "dir"),
                    arg(args, "question"),
                    arg(args, "report"),
                    &claims,
                )),
            }
        }
        "" => Err("action is required: extract, record or write".into()),
        other => Err(format!(
            "unknown action {other}; use extract, record or write"
        )),
    }
}

fn render_claims(claims: &[Claim]) -> String {
    let mut out = format!(
        "{} atomic claims (method statements and short fragments dropped, at most {CLAIM_MAX} kept).\n",
        claims.len()
    );
    for (index, claim) in claims.iter().enumerate() {
        let _ = writeln!(out, "{}. [{}] {}", index + 1, claim.location, claim.text);
    }
    let _ = write!(
        out,
        "\nVerify each claim with web_search (paper_search for scientific ones), at most {QUERY_MAX} queries per \
         claim, then call fact_check with action \"record\" and the verdicts."
    );
    out
}

fn render_scored(claims: &[Claim]) -> String {
    let summary = score(claims);
    let mut out = format!(
        "{} of {} claims supported, {} not supported, {} unclear.\n",
        summary.supported, summary.total, summary.not_supported, summary.unclear
    );
    for claim in claims.iter().filter(|c| c.verdict != "supported") {
        let source = claim
            .sources
            .first()
            .map_or("no source", |(url, _)| url.as_str());
        let _ = writeln!(
            out,
            "- {} [{}] -> {} ({source})",
            claim.text, claim.location, claim.verdict
        );
    }
    if summary.not_supported > 0 {
        let _ = write!(
            out,
            "\nRevise the {} not-supported claim(s) once: re-source, correct or remove each, then write the result \
             with action \"write\".",
            summary.not_supported
        );
    }
    out
}

fn write_files(
    root: &std::path::Path,
    question: &str,
    report: &str,
    claims: &[Claim],
) -> Result<String, String> {
    std::fs::create_dir_all(root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let json = factcheck_json(question, report, claims);
    let rendered = serde_json::to_string_pretty(&json).unwrap_or_default();
    let target = root.join("factcheck.json");
    std::fs::write(&target, rendered)
        .map_err(|e| format!("cannot write {}: {e}", target.display()))?;
    let summary = score(claims);
    let mut out = format!(
        "Wrote {} ({} of {} claims supported, {} not supported, {} unclear).",
        target.display(),
        summary.supported,
        summary.total,
        summary.not_supported,
        summary.unclear
    );
    let tex = root.join("report.tex");
    match std::fs::read_to_string(&tex) {
        Ok(text) if text.contains("\\section*{Fact-check}") => {
            out.push_str(" report.tex already has the Fact-check appendix; left unchanged.");
        }
        Ok(text) => {
            let table = appendix(claims);
            let updated = match text.rfind("\\end{document}") {
                Some(at) => format!("{}{table}\n{}", &text[..at], &text[at..]),
                None => format!("{text}\n{table}"),
            };
            std::fs::write(&tex, updated)
                .map_err(|e| format!("cannot write {}: {e}", tex.display()))?;
            out.push_str(" Appended the Fact-check appendix to report.tex.");
        }
        Err(_) => {
            let _ = write!(
                out,
                " No report.tex in that folder; add this table to the report:\n{}",
                appendix(claims)
            );
        }
    }
    Ok(out)
}

fn render_output(dir: &str, question: &str, report: &str, claims: &[Claim]) -> String {
    let mut out = String::new();
    let json = factcheck_json(question, report, claims);
    let rendered = serde_json::to_string_pretty(&json).unwrap_or_default();
    let _ = writeln!(out, "factcheck.json:\n{rendered}");
    if dir.is_empty() {
        let _ = writeln!(
            out,
            "\nNo dir given: write it to research/<slug>/factcheck.json."
        );
    } else {
        let _ = writeln!(
            out,
            "\nWrite it to {dir}/factcheck.json and append the table below to report.tex once, before \
             \\end{{document}}."
        );
    }
    let _ = write!(out, "\n{}", appendix(claims));
    out
}

pub fn factcheck_json(question: &str, report: &str, claims: &[Claim]) -> Value {
    let summary = score(claims);
    let entries = claims
        .iter()
        .map(|claim| {
            json!({
                "text": claim.text,
                "location": claim.location,
                "verdict": claim.verdict,
                "reason": claim.reason,
                "sources": claim.sources.iter().map(|(url, title)| json!({"url": url, "title": title})).collect::<Vec<_>>(),
                "queries": claim.queries
            })
        })
        .collect::<Vec<_>>();
    json!({
        "question": question,
        "report": report,
        "summary": {
            "total": summary.total,
            "supported": summary.supported,
            "notSupported": summary.not_supported,
            "unclear": summary.unclear,
            "unchecked": 0
        },
        "claims": entries
    })
}

/// The LaTeX appendix: claim, verdict and the source that settled it.
pub fn appendix(claims: &[Claim]) -> String {
    let summary = score(claims);
    let mut out = String::new();
    let _ = writeln!(out, "\\section*{{Fact-check}}");
    let _ = writeln!(
        out,
        "{} of {} claims supported, {} not supported, {} unclear. Each claim, its verdict and the source that \
         settled it:",
        summary.supported, summary.total, summary.not_supported, summary.unclear
    );
    let _ = writeln!(
        out,
        "\\begin{{longtable}}{{p{{0.46\\linewidth}}p{{0.14\\linewidth}}p{{0.32\\linewidth}}}}"
    );
    let _ = writeln!(out, "\\toprule\nClaim & Verdict & Source \\\\\n\\midrule");
    for claim in claims {
        let _ = writeln!(
            out,
            "{} & {} & {} \\\\",
            escape(&claim.text),
            verdict_label(&claim.verdict),
            source_cell(claim)
        );
    }
    let _ = writeln!(out, "\\bottomrule\n\\end{{longtable}}");
    out
}

fn source_cell(claim: &Claim) -> String {
    match claim.sources.first() {
        Some((url, title)) if !title.is_empty() => {
            format!("\\url{{{}}}", escape_url(url, title))
        }
        Some((url, _)) => format!("\\url{{{}}}", escape_url(url, url)),
        None => "no source".to_owned(),
    }
}

fn verdict_label(verdict: &str) -> &'static str {
    match verdict {
        "supported" => "supported",
        "notSupported" => "not supported",
        _ => "unclear",
    }
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '$' => out.push_str("\\$"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\\' => out.push_str("\\textbackslash{}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            other => out.push(other),
        }
    }
    out
}

/// A URL inside `\url{}` with the characters that break the command blanked out, plus the page title.
fn escape_url(url: &str, title: &str) -> String {
    let link = url
        .chars()
        .map(|c| match c {
            '&' | '%' | '$' | '#' | '_' | '{' | '}' | '\\' => ' ',
            other => other,
        })
        .collect::<String>();
    if title.is_empty() || title == url {
        link
    } else {
        format!("{link} ({})", escape(title))
    }
}

#[cfg(test)]
mod tests;
