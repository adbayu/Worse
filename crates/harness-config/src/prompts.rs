use std::path::Path;

pub const PROMPTS_DIR: &str = "prompts";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptId {
    ApiAgentSystem,
}

impl PromptId {
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::ApiAgentSystem => "api-agent-system.md",
        }
    }

    #[must_use]
    pub const fn embedded(self) -> &'static str {
        match self {
            Self::ApiAgentSystem => include_str!("../assets/prompts/api-agent-system.md"),
        }
    }
}

#[must_use]
pub fn prompt(id: PromptId) -> String {
    crate::paths::harness_home()
        .and_then(|home| user_override(&home, id))
        .unwrap_or_else(|| normalize(id.embedded()))
}

fn user_override(home: &Path, id: PromptId) -> Option<String> {
    let text = std::fs::read_to_string(home.join(PROMPTS_DIR).join(id.file_name())).ok()?;
    let text = normalize(&text);
    (!text.trim().is_empty()).then_some(text)
}

fn normalize(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    match text.strip_suffix('\n') {
        Some(stripped) => stripped.to_owned(),
        None => text,
    }
}

#[must_use]
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let key = &after[..close];
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value, close))
        });
        if let Some((value, close)) = value {
            out.push_str(value);
            rest = &after[close + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fills_known_keys_once() {
        let out = render("a {x} b {y} {unknown} {", &[("x", "{y}"), ("y", "2")]);
        assert_eq!(out, "a {y} b 2 {unknown} {");
    }

    #[test]
    fn embedded_prompt_has_no_trailing_newline() {
        let text = normalize(PromptId::ApiAgentSystem.embedded());
        assert!(!text.contains('\r'));
        assert!(text.starts_with("You are {label}"));
        assert!(text.ends_with("assumptions."));
    }
}
